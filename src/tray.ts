import { ChildProcess, spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { getLogger } from './utils/logger';
import { statusFilePath } from './utils/status-file';

export interface TrayOptions {
  iconPath?: string;
  rpcButtonsPath: string;
  /** Command string written to HKCU Run when the user enables "Start with Windows". */
  startupCommand: string;
  onButtonsChanged: () => void;
  onQuit: () => void;
}

const RUN_REGISTRY_NAME = 'CodexRichPresence';

interface TrayHandle {
  stop: () => void;
}

/**
 * Spawns a hidden PowerShell that owns a Win32 NotifyIcon (tray icon).
 * The tray process polls the status file (written by the daemon) to refresh
 * its tooltip + menu label. Emits line-based commands on stdout we parse here:
 *   CMD:QUIT    — user clicked Quit
 */
export function startTray(options: TrayOptions): TrayHandle {
  const log = getLogger();
  const scriptPath = writeTrayScript(options.iconPath, options.rpcButtonsPath, options.startupCommand);

  const child: ChildProcess = spawn(
    `${process.env.SystemRoot ?? 'C:\\Windows'}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe`,
    ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-WindowStyle', 'Hidden', '-File', scriptPath],
    { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] },
  );

  let buffer = '';
  child.stdout?.on('data', (chunk: Buffer) => {
    buffer += chunk.toString('utf8');
    let idx: number;
    while ((idx = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, idx).trim();
      buffer = buffer.slice(idx + 1);
      if (!line) continue;
      if (line === 'CMD:QUIT') {
        log.info('tray: quit requested');
        options.onQuit();
      } else if (line === 'CMD:BUTTONS_CHANGED') {
        log.info('tray: rpc buttons changed');
        options.onButtonsChanged();
      } else {
        log.debug({ line }, 'tray: unexpected message');
      }
    }
  });

  child.stderr?.on('data', (chunk: Buffer) => {
    const text = chunk.toString('utf8').trim();
    if (text) log.warn({ text: text.slice(0, 500) }, 'tray: stderr');
  });

  child.on('exit', (code) => {
    log.warn({ code }, 'tray: powershell exited');
  });

  return {
    stop: (): void => {
      try {
        child.kill('SIGKILL');
      } catch {
        /* ignore */
      }
    },
  };
}

function psEscape(value: string): string {
  return value.replace(/\\/g, '\\\\').replace(/'/g, "''");
}

function writeTrayScript(
  iconPath: string | undefined,
  rpcButtonsPath: string,
  startupCommand: string,
): string {
  const dir = path.join(
    process.env.LOCALAPPDATA ?? path.join(os.homedir(), 'AppData', 'Local'),
    'codex-rich-presence',
  );
  fs.mkdirSync(dir, { recursive: true });
  const scriptPath = path.join(dir, 'tray.ps1');
  const statusFile = statusFilePath();

  const iconPs = psEscape(iconPath ?? '');
  const statusPs = psEscape(statusFile);
  const rpcButtonsPs = psEscape(rpcButtonsPath);
  const startupCommandPs = psEscape(startupCommand);
  const runKeyName = RUN_REGISTRY_NAME;

  const body = `Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$ErrorActionPreference = 'SilentlyContinue'
$statusFile = '${statusPs}'
$rpcButtonsFile = '${rpcButtonsPs}'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()

$UiBack = [System.Drawing.Color]::FromArgb(8, 12, 11)
$UiPanel = [System.Drawing.Color]::FromArgb(12, 18, 16)
$UiInput = [System.Drawing.Color]::FromArgb(3, 6, 5)
$UiButton = [System.Drawing.Color]::FromArgb(15, 26, 22)
$UiButtonHover = [System.Drawing.Color]::FromArgb(21, 43, 34)
$UiBorder = [System.Drawing.Color]::FromArgb(44, 72, 58)
$UiText = [System.Drawing.Color]::FromArgb(218, 255, 228)
$UiMuted = [System.Drawing.Color]::FromArgb(112, 172, 137)
$UiAccent = [System.Drawing.Color]::FromArgb(49, 207, 116)
$UiFont = New-Object System.Drawing.Font('Consolas', 9)
$UiFontBold = New-Object System.Drawing.Font('Consolas', 9, [System.Drawing.FontStyle]::Bold)

function Set-DarkText {
  param([System.Windows.Forms.Control] $Control, [bool] $Muted = $false)
  $Control.BackColor = $UiBack
  $Control.ForeColor = $(if ($Muted) { $UiMuted } else { $UiText })
  $Control.Font = $UiFont
}

function Set-DarkGroup {
  param([System.Windows.Forms.Control] $Control)
  $Control.BackColor = $UiPanel
  $Control.ForeColor = $UiText
  $Control.Font = $UiFontBold
}

function Set-DarkInput {
  param([System.Windows.Forms.TextBox] $Control)
  $Control.BackColor = $UiInput
  $Control.ForeColor = $UiText
  $Control.BorderStyle = [System.Windows.Forms.BorderStyle]::FixedSingle
  $Control.Font = $UiFont
}

function Set-DarkButton {
  param([System.Windows.Forms.Button] $Control, [bool] $Accent = $false)
  $Control.FlatStyle = [System.Windows.Forms.FlatStyle]::Flat
  $Control.BackColor = $(if ($Accent) { $UiAccent } else { $UiButton })
  $Control.ForeColor = $(if ($Accent) { [System.Drawing.Color]::FromArgb(2, 10, 6) } else { $UiText })
  $Control.Font = $UiFont
  $Control.FlatAppearance.BorderColor = $UiBorder
  $Control.FlatAppearance.MouseOverBackColor = $(if ($Accent) { [System.Drawing.Color]::FromArgb(76, 245, 143) } else { $UiButtonHover })
  $Control.FlatAppearance.MouseDownBackColor = [System.Drawing.Color]::FromArgb(6, 19, 13)
}

function Set-DarkMenuItem {
  param([System.Windows.Forms.ToolStripItem] $Item)
  $Item.BackColor = $UiPanel
  $Item.ForeColor = $UiText
}

# Build a tray-sized icon (16x16) from the source ICO with high-quality
# bicubic resampling. Using the raw Icon would let Windows downscale with
# nearest-neighbour, which looks blocky in the system tray.
function New-TrayIcon {
  param([string] $Path, [int] $Size = 16)
  $src = New-Object System.Drawing.Icon($Path)
  $bmp = New-Object System.Drawing.Bitmap($Size, $Size)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
  $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.DrawImage($src.ToBitmap(), 0, 0, $Size, $Size)
  $g.Dispose()
  $src.Dispose()
  $handle = $bmp.GetHicon()
  return [System.Drawing.Icon]::FromHandle($handle)
}

function Read-RpcButtons {
  $defaults = @(
    [pscustomobject]@{ label = ''; url = '' },
    [pscustomobject]@{ label = ''; url = '' }
  )
  try {
    if (-not (Test-Path $rpcButtonsFile)) { return $defaults }
    $json = Get-Content -Path $rpcButtonsFile -Raw -Encoding UTF8 | ConvertFrom-Json
    if (-not $json.buttons) { return $defaults }
    for ($i = 0; $i -lt [Math]::Min(2, $json.buttons.Count); $i++) {
      $defaults[$i].label = [string] $json.buttons[$i].label
      $defaults[$i].url = [string] $json.buttons[$i].url
    }
  } catch { }
  return $defaults
}

function Read-RpcMode {
  try {
    if (-not (Test-Path $rpcButtonsFile)) { return 'playing' }
    $json = Get-Content -Path $rpcButtonsFile -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($json.mode -eq 'watching' -or $json.mode -eq 'tv') { return 'watching' }
    if ($json.mode -eq 'listening' -or $json.mode -eq 'listen') { return 'listening' }
    if ($json.mode -eq 'competing' -or $json.mode -eq 'compete') { return 'competing' }
  } catch { }
  return 'playing'
}

function Save-RpcButtons {
  param([string] $Mode, [string[]] $Labels, [string[]] $Urls)
  try {
    $dir = [System.IO.Path]::GetDirectoryName($rpcButtonsFile)
    if ($dir -and -not (Test-Path $dir)) { New-Item -Path $dir -ItemType Directory -Force | Out-Null }
    $items = New-Object System.Collections.ArrayList
    for ($i = 0; $i -lt 2; $i++) {
      $label = $Labels[$i].Trim()
      $url = $Urls[$i].Trim()
      if ($label -and $url) {
        [void] $items.Add([ordered]@{ label = $label; url = $url })
      }
    }
    $json = [ordered]@{ mode = $Mode; buttons = $items } | ConvertTo-Json -Depth 4
    [System.IO.File]::WriteAllText($rpcButtonsFile, $json, [System.Text.UTF8Encoding]::new($false))
    [Console]::Out.WriteLine('CMD:BUTTONS_CHANGED')
    [Console]::Out.Flush()
  } catch {
    [System.Windows.Forms.MessageBox]::Show(
      "Failed to save RPC buttons:\`n\$(\$_.Exception.Message)",
      'Codex Rich Presence', 'OK', 'Warning') | Out-Null
  }
}

function Show-RpcButtonsDialog {
  $current = Read-RpcButtons
  $currentMode = Read-RpcMode
  $form = New-Object System.Windows.Forms.Form
  $form.Text = 'codex-rpc://settings'
  $form.StartPosition = 'CenterScreen'
  $form.FormBorderStyle = 'FixedDialog'
  $form.MaximizeBox = $false
  $form.MinimizeBox = $false
  $form.ClientSize = New-Object System.Drawing.Size(560, 340)
  $form.BackColor = $UiBack
  $form.ForeColor = $UiText
  $form.Font = $UiFont

  $buttonsGroup = New-Object System.Windows.Forms.GroupBox
  $buttonsGroup.Text = '> rpc.buttons'
  $buttonsGroup.Location = New-Object System.Drawing.Point(16, 14)
  $buttonsGroup.Size = New-Object System.Drawing.Size(528, 138)
  Set-DarkGroup $buttonsGroup

  $buttonCol = New-Object System.Windows.Forms.Label
  $buttonCol.Location = New-Object System.Drawing.Point(14, 25)
  $buttonCol.Size = New-Object System.Drawing.Size(70, 18)
  $buttonCol.Text = '$slot'
  Set-DarkText $buttonCol $true
  $labelCol = New-Object System.Windows.Forms.Label
  $labelCol.Location = New-Object System.Drawing.Point(92, 25)
  $labelCol.Size = New-Object System.Drawing.Size(125, 18)
  $labelCol.Text = '$label'
  Set-DarkText $labelCol $true
  $urlCol = New-Object System.Windows.Forms.Label
  $urlCol.Location = New-Object System.Drawing.Point(232, 25)
  $urlCol.Size = New-Object System.Drawing.Size(270, 18)
  $urlCol.Text = '$url'
  Set-DarkText $urlCol $true

  $row1 = New-Object System.Windows.Forms.Label
  $row1.Location = New-Object System.Drawing.Point(14, 55)
  $row1.Size = New-Object System.Drawing.Size(70, 18)
  $row1.Text = 'btn[0]'
  Set-DarkText $row1

  $label1 = New-Object System.Windows.Forms.TextBox
  $label1.Location = New-Object System.Drawing.Point(92, 52)
  $label1.Size = New-Object System.Drawing.Size(125, 23)
  $label1.Text = $current[0].label
  Set-DarkInput $label1
  try { $label1.PlaceholderText = 'Open Codex' } catch { }
  $url1 = New-Object System.Windows.Forms.TextBox
  $url1.Location = New-Object System.Drawing.Point(232, 52)
  $url1.Size = New-Object System.Drawing.Size(275, 23)
  $url1.Text = $current[0].url
  Set-DarkInput $url1
  try { $url1.PlaceholderText = 'https://...' } catch { }

  $row2 = New-Object System.Windows.Forms.Label
  $row2.Location = New-Object System.Drawing.Point(14, 94)
  $row2.Size = New-Object System.Drawing.Size(70, 18)
  $row2.Text = 'btn[1]'
  Set-DarkText $row2

  $label2 = New-Object System.Windows.Forms.TextBox
  $label2.Location = New-Object System.Drawing.Point(92, 91)
  $label2.Size = New-Object System.Drawing.Size(125, 23)
  $label2.Text = $current[1].label
  Set-DarkInput $label2
  try { $label2.PlaceholderText = 'Usage' } catch { }
  $url2 = New-Object System.Windows.Forms.TextBox
  $url2.Location = New-Object System.Drawing.Point(232, 91)
  $url2.Size = New-Object System.Drawing.Size(275, 23)
  $url2.Text = $current[1].url
  Set-DarkInput $url2
  try { $url2.PlaceholderText = 'https://...' } catch { }

  [void] $buttonsGroup.Controls.AddRange(@($buttonCol, $labelCol, $urlCol, $row1, $label1, $url1, $row2, $label2, $url2))

  $modeGroup = New-Object System.Windows.Forms.GroupBox
  $modeGroup.Text = '> rpc.mode'
  $modeGroup.Location = New-Object System.Drawing.Point(16, 162)
  $modeGroup.Size = New-Object System.Drawing.Size(528, 58)
  Set-DarkGroup $modeGroup

  $modeLabel = New-Object System.Windows.Forms.Label
  $modeLabel.Location = New-Object System.Drawing.Point(14, 27)
  $modeLabel.Size = New-Object System.Drawing.Size(80, 18)
  $modeLabel.Text = '$mode'
  Set-DarkText $modeLabel

  $modeSelect = New-Object System.Windows.Forms.ComboBox
  $modeSelect.Location = New-Object System.Drawing.Point(92, 23)
  $modeSelect.Size = New-Object System.Drawing.Size(150, 23)
  $modeSelect.DropDownStyle = [System.Windows.Forms.ComboBoxStyle]::DropDownList
  $modeSelect.FlatStyle = [System.Windows.Forms.FlatStyle]::Flat
  $modeSelect.BackColor = $UiInput
  $modeSelect.ForeColor = $UiText
  $modeSelect.Font = $UiFont
  [void] $modeSelect.Items.AddRange(@('Playing', 'Watching (TV)', 'Listening', 'Competing'))
  if ($currentMode -eq 'watching') { $modeSelect.SelectedIndex = 1 }
  elseif ($currentMode -eq 'listening') { $modeSelect.SelectedIndex = 2 }
  elseif ($currentMode -eq 'competing') { $modeSelect.SelectedIndex = 3 }
  else { $modeSelect.SelectedIndex = 0 }

  $hint = New-Object System.Windows.Forms.Label
  $hint.Location = New-Object System.Drawing.Point(258, 19)
  $hint.Size = New-Object System.Drawing.Size(250, 36)
  $hint.Text = 'buttons emit only when mode == watching'
  Set-DarkText $hint $true

  function Get-SelectedRpcMode {
    if ($modeSelect.SelectedIndex -eq 1) { return 'watching' }
    if ($modeSelect.SelectedIndex -eq 2) { return 'listening' }
    if ($modeSelect.SelectedIndex -eq 3) { return 'competing' }
    return 'playing'
  }

  [void] $modeGroup.Controls.AddRange(@($modeLabel, $modeSelect, $hint))

  $presetsGroup = New-Object System.Windows.Forms.GroupBox
  $presetsGroup.Text = '> presets'
  $presetsGroup.Location = New-Object System.Drawing.Point(16, 230)
  $presetsGroup.Size = New-Object System.Drawing.Size(528, 62)
  Set-DarkGroup $presetsGroup

  function Set-PresetButton {
    param([string] $Label, [string] $Url)
    if (-not $label1.Text.Trim()) {
      $label1.Text = $Label
      $url1.Text = $Url
    } elseif (-not $label2.Text.Trim()) {
      $label2.Text = $Label
      $url2.Text = $Url
    } else {
      $label1.Text = $Label
      $url1.Text = $Url
    }
  }

  $presetCodex = New-Object System.Windows.Forms.Button
  $presetCodex.Text = './codex'
  $presetCodex.Location = New-Object System.Drawing.Point(14, 24)
  $presetCodex.Size = New-Object System.Drawing.Size(110, 25)
  Set-DarkButton $presetCodex
  $presetCodex.Add_Click({ Set-PresetButton 'Open Codex' 'https://chatgpt.com/codex' })

  $presetUsage = New-Object System.Windows.Forms.Button
  $presetUsage.Text = './usage'
  $presetUsage.Location = New-Object System.Drawing.Point(134, 24)
  $presetUsage.Size = New-Object System.Drawing.Size(110, 25)
  Set-DarkButton $presetUsage
  $presetUsage.Add_Click({ Set-PresetButton 'Usage' 'https://chatgpt.com/codex/settings/analytics' })

  $presetGithub = New-Object System.Windows.Forms.Button
  $presetGithub.Text = './repo'
  $presetGithub.Location = New-Object System.Drawing.Point(254, 24)
  $presetGithub.Size = New-Object System.Drawing.Size(110, 25)
  Set-DarkButton $presetGithub
  $presetGithub.Add_Click({ Set-PresetButton 'GitHub repo' 'https://github.com/inerthel-agi/codex-rpc' })

  [void] $presetsGroup.Controls.AddRange(@($presetCodex, $presetUsage, $presetGithub))

  function Apply-RpcButtonSettings {
    $mode = Get-SelectedRpcMode
    Save-RpcButtons -Mode $mode -Labels @($label1.Text, $label2.Text) -Urls @($url1.Text, $url2.Text)
  }

  $clear = New-Object System.Windows.Forms.Button
  $clear.Text = 'clear'
  $clear.Location = New-Object System.Drawing.Point(16, 308)
  $clear.Size = New-Object System.Drawing.Size(75, 23)
  Set-DarkButton $clear
  $clear.Add_Click({
    $label1.Text = ''
    $url1.Text = ''
    $label2.Text = ''
    $url2.Text = ''
  })

  $save = New-Object System.Windows.Forms.Button
  $save.Text = 'save'
  $save.Location = New-Object System.Drawing.Point(298, 308)
  $save.Size = New-Object System.Drawing.Size(75, 23)
  Set-DarkButton $save $true
  $save.DialogResult = [System.Windows.Forms.DialogResult]::OK
  $apply = New-Object System.Windows.Forms.Button
  $apply.Text = 'apply'
  $apply.Location = New-Object System.Drawing.Point(382, 308)
  $apply.Size = New-Object System.Drawing.Size(75, 23)
  Set-DarkButton $apply
  $apply.Add_Click({ Apply-RpcButtonSettings })
  $cancel = New-Object System.Windows.Forms.Button
  $cancel.Text = 'exit'
  $cancel.Location = New-Object System.Drawing.Point(466, 308)
  $cancel.Size = New-Object System.Drawing.Size(75, 23)
  Set-DarkButton $cancel
  $cancel.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
  $form.AcceptButton = $save
  $form.CancelButton = $cancel

  function Sync-RpcButtonControls {
    $enabled = ((Get-SelectedRpcMode) -eq 'watching')
    foreach ($control in @($label1, $url1, $label2, $url2, $presetCodex, $presetUsage, $presetGithub, $clear)) {
      $control.Enabled = $enabled
    }
  }
  $modeSelect.Add_SelectedIndexChanged({ Sync-RpcButtonControls })
  Sync-RpcButtonControls

  [void] $form.Controls.AddRange(@($buttonsGroup, $modeGroup, $presetsGroup, $clear, $save, $apply, $cancel))
  if ($form.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
    Apply-RpcButtonSettings
  }
  $form.Dispose()
}

$notifyIcon = New-Object System.Windows.Forms.NotifyIcon
$notifyIcon.Text = 'Codex Rich Presence'
$iconPath = '${iconPs}'
if ($iconPath -and (Test-Path $iconPath)) {
  try { $notifyIcon.Icon = New-TrayIcon -Path $iconPath -Size 16 }
  catch { $notifyIcon.Icon = [System.Drawing.SystemIcons]::Application }
} else {
  $notifyIcon.Icon = [System.Drawing.SystemIcons]::Application
}
$notifyIcon.Visible = $true

$menu = New-Object System.Windows.Forms.ContextMenuStrip
$menu.BackColor = $UiPanel
$menu.ForeColor = $UiText
$menu.ShowImageMargin = $false
$menu.Font = $UiFont

$titleItem = $menu.Items.Add('Codex Rich Presence')
$titleItem.Enabled = $false
$titleItem.Font = New-Object System.Drawing.Font($titleItem.Font, [System.Drawing.FontStyle]::Bold)
Set-DarkMenuItem $titleItem

$stateItem = $menu.Items.Add('Starting...')
$stateItem.Enabled = $false
Set-DarkMenuItem $stateItem

$modelItem = $menu.Items.Add('')
$modelItem.Enabled = $false
$modelItem.Visible = $false
Set-DarkMenuItem $modelItem

$usageItem = $menu.Items.Add('')
$usageItem.Enabled = $false
$usageItem.Visible = $false
Set-DarkMenuItem $usageItem

$discordItem = $menu.Items.Add('')
$discordItem.Enabled = $false
$discordItem.Visible = $false
Set-DarkMenuItem $discordItem

[void] $menu.Items.Add('-')

$buttonsItem = $menu.Items.Add('RPC Buttons...')
Set-DarkMenuItem $buttonsItem
$buttonsItem.Add_Click({ Show-RpcButtonsDialog })

[void] $menu.Items.Add('-')

$runKey = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run'
$runValueName = '${runKeyName}'
$startupCommand = '${startupCommandPs}'

$startupItem = New-Object System.Windows.Forms.ToolStripMenuItem('Start with Windows')
$startupItem.CheckOnClick = $true
Set-DarkMenuItem $startupItem
try {
  $existing = Get-ItemProperty -Path $runKey -Name $runValueName -ErrorAction SilentlyContinue
  if ($existing) { $startupItem.Checked = $true }
} catch { }
[void] $menu.Items.Add($startupItem)

$startupItem.Add_Click({
  try {
    if (-not (Test-Path $runKey)) {
      New-Item -Path $runKey -Force | Out-Null
    }
    if ($startupItem.Checked) {
      Set-ItemProperty -Path $runKey -Name $runValueName -Value $startupCommand -Type String
    } else {
      Remove-ItemProperty -Path $runKey -Name $runValueName -ErrorAction SilentlyContinue
    }
  } catch {
    [System.Windows.Forms.MessageBox]::Show(
      "Failed to update startup setting:\`n\$(\$_.Exception.Message)",
      'Codex Rich Presence', 'OK', 'Warning') | Out-Null
    $startupItem.Checked = -not $startupItem.Checked
  }
})

[void] $menu.Items.Add('-')

$quitItem = $menu.Items.Add('Quit')
Set-DarkMenuItem $quitItem

$notifyIcon.ContextMenuStrip = $menu

$quitItem.Add_Click({
  [Console]::Out.WriteLine('CMD:QUIT')
  [Console]::Out.Flush()
  $notifyIcon.Visible = $false
  [System.Windows.Forms.Application]::Exit()
})

$notifyIcon.Add_MouseDoubleClick({
  # Surface the tooltip as a balloon on double-click — tiny UX touch.
  $notifyIcon.ShowBalloonTip(2000, 'Codex Rich Presence', $stateItem.Text, [System.Windows.Forms.ToolTipIcon]::Info)
})

$timer = New-Object System.Windows.Forms.Timer
$timer.Interval = 3000
$timer.Add_Tick({
  try {
    if (Test-Path $statusFile) {
      $line = [System.IO.File]::ReadAllText($statusFile, [System.Text.UTF8Encoding]::new()).Trim()
      if ($line) {
        $parts = $line -split '\\|', 4
        $stateItem.Text = $parts[0]
        if ($parts.Length -gt 1 -and $parts[1]) {
          $modelItem.Text = $parts[1]
          $modelItem.Visible = $true
        } else {
          $modelItem.Visible = $false
        }
        if ($parts.Length -gt 2 -and $parts[2]) {
          $usageItem.Text = $parts[2]
          $usageItem.Visible = $true
        } else {
          $usageItem.Visible = $false
        }
        if ($parts.Length -gt 3 -and $parts[3]) {
          $discordItem.Text = $parts[3]
          $discordItem.Visible = $true
        } else {
          $discordItem.Visible = $false
        }
        $tooltipSource = ($parts -join ' | ')
        $tooltipLen = [Math]::Min(63, $tooltipSource.Length)
        $notifyIcon.Text = 'Codex RP | ' + $tooltipSource.Substring(0, $tooltipLen)
      }
    }
  } catch { }
})
$timer.Start()

[System.Windows.Forms.Application]::Run()
`;

  fs.writeFileSync(scriptPath, body, 'utf8');
  return scriptPath;
}
