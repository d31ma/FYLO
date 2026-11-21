# FYLO test disks (ADR 0033 section 12) for Windows. Windows PowerShell 5.1 or later.
#
#   test-disk.ps1 -Action Make -Folder D:\fylo-test -Letter T [-SizeGiB 8]
#   test-disk.ps1 -Action Online -Serial TEST-DISK -Letter T
#   test-disk.ps1 -Action Run -Letter T [-Owner <name>] -Script <file.ps1> [-Arguments <a>,<b>]
#   test-disk.ps1 -Action Detach -Folder D:\fylo-test [-Delete]
#   test-disk.ps1 -Action Status -Letter T
#
# Make: creates a sparse (dynamically expanding) VHDX in -Folder, which must be
# on a disk other than the Windows disk (GitHub runners: D:), attaches it,
# formats it NTFS and gives it drive letter -Letter. Laptops and GitHub runners.
#
# Online: windows-vm's test disk, attached by Platform's testvm as a second
# disk. Finds the disk by its serial number (or, for a USB test disk, the only
# USB disk), brings it online and writable, and on first use only (a disk with
# no partitions) formats it NTFS. It never touches the boot or system disk, and
# never formats a disk that already has partitions.
#
# Run: runs one job with FYLO_TEST_ROOT=<letter>:\roots\<owner>-<host>-<pid>,
# after a write probe, a 1 GiB free check and a sweep of job folders older than
# 24 hours whose process is gone. The folder is removed on every exit path.
#
# On failure it prints "FYLO test disk not ready: <reason>" and exits 1.

param(
  [Parameter(Mandatory = $true)][ValidateSet('Make', 'Online', 'Run', 'Detach', 'Status')][string]$Action,
  [string]$Folder = '',
  [string]$Letter = 'T',
  [string]$Serial = 'TEST-DISK',
  [string]$Owner = '',
  [string]$Script = '',
  [string[]]$Arguments = @(),
  [int]$SizeGiB = 8,
  [switch]$Delete
)

$ErrorActionPreference = 'Stop'

class FyloTestDisk {
  static [string] $ImageName = 'fylo-test-disk.vhdx'
  static [string] $VolumeLabel = 'FYLOTEST'
  static [long] $MinimumFreeBytes = 1GB
  static [int] $StaleMinutes = 1440

  static [void] Say([string]$text) {
    [Console]::Error.WriteLine("=== fylo test disk $(Get-Date -Format 'HH:mm:ss'): $text")
  }

  static [void] Fail([string]$reason) {
    throw "FYLO test disk not ready: $reason"
  }

  static [string] Drive([string]$letter) {
    return $letter.TrimEnd(':') + ':\'
  }

  static [int] DiskNumberOf([string]$path) {
    $letter = (Resolve-Path -LiteralPath $path).Drive.Name
    return (Get-Partition -DriveLetter $letter | Get-Disk).Number
  }

  static [void] RequireDataDisk([string]$folder) {
    $windows = [FyloTestDisk]::DiskNumberOf($env:SystemDrive + '\')
    $here = [FyloTestDisk]::DiskNumberOf($folder)
    if ($windows -eq $here) {
      [FyloTestDisk]::Fail("the image folder $folder must be on a disk that is not the Windows disk (disk $windows)")
    }
  }

  static [string] ImagePath([string]$folder) {
    return Join-Path $folder ([FyloTestDisk]::ImageName)
  }

  static [void] CreateImage([string]$image, [int]$sizeGiB) {
    [FyloTestDisk]::Say("creating $image (dynamically expanding VHDX, $sizeGiB GiB maximum)")
    $script = Join-Path $env:TEMP "fylo-test-disk-$PID.txt"
    Set-Content -LiteralPath $script -Encoding ASCII -Value "create vdisk file=`"$image`" maximum=$($sizeGiB * 1024) type=expandable"
    diskpart /s $script | Out-Host
    $status = $global:LASTEXITCODE
    Remove-Item -LiteralPath $script -Force
    if ($status -ne 0) { [FyloTestDisk]::Fail("diskpart could not create $image") }
  }

  static [object] AttachImage([string]$image) {
    [FyloTestDisk]::Say("attaching $image")
    $attached = Get-DiskImage -ImagePath $image
    if (-not $attached.Attached) { $attached = Mount-DiskImage -ImagePath $image -NoDriveLetter -PassThru }
    return ($attached | Get-Disk)
  }

  static [void] RequireNotWindowsDisk([object]$disk) {
    if ($disk.IsBoot -or $disk.IsSystem) { [FyloTestDisk]::Fail("disk $($disk.Number) is the boot or system disk") }
  }

  static [void] MakeWritable([object]$disk) {
    if ($disk.IsOffline) { Set-Disk -Number $disk.Number -IsOffline $false }
    if ($disk.IsReadOnly) { Set-Disk -Number $disk.Number -IsReadOnly $false }
  }

  static [void] FormatIfNew([object]$disk, [string]$letter) {
    $fresh = Get-Disk -Number $disk.Number
    if ($fresh.PartitionStyle -ne 'RAW') { return }
    [FyloTestDisk]::Say("disk $($disk.Number) is new: GPT, one NTFS partition, label $([FyloTestDisk]::VolumeLabel)")
    Initialize-Disk -Number $disk.Number -PartitionStyle GPT
    $partition = New-Partition -DiskNumber $disk.Number -UseMaximumSize -DriveLetter $letter.TrimEnd(':')
    Format-Volume -Partition $partition -FileSystem NTFS -NewFileSystemLabel ([FyloTestDisk]::VolumeLabel) -Confirm:$false | Out-Null
  }

  static [object] DataPartition([object]$disk) {
    $partitions = @(Get-Partition -DiskNumber $disk.Number | Where-Object { $_.Type -eq 'Basic' -or $_.Type -eq 'IFS' })
    if ($partitions.Count -ne 1) { [FyloTestDisk]::Fail("disk $($disk.Number) has $($partitions.Count) data partitions; one is expected") }
    return $partitions[0]
  }

  static [void] AssignLetter([object]$disk, [string]$letter) {
    $partition = [FyloTestDisk]::DataPartition($disk)
    $wanted = $letter.TrimEnd(':')
    if ($partition.DriveLetter -eq $wanted) { return }
    $used = Get-Volume -DriveLetter $wanted -ErrorAction SilentlyContinue
    if ($used) { [FyloTestDisk]::Fail("drive letter $wanted is already used by another volume") }
    Set-Partition -DiskNumber $disk.Number -PartitionNumber $partition.PartitionNumber -NewDriveLetter $wanted
  }

  static [void] Prepare([object]$disk, [string]$letter) {
    [FyloTestDisk]::RequireNotWindowsDisk($disk)
    [FyloTestDisk]::MakeWritable($disk)
    [FyloTestDisk]::FormatIfNew($disk, $letter)
    [FyloTestDisk]::AssignLetter($disk, $letter)
    [FyloTestDisk]::RequireWritable($letter)
  }

  static [void] Make([string]$folder, [string]$letter, [int]$sizeGiB) {
    New-Item -ItemType Directory -Force -Path $folder | Out-Null
    [FyloTestDisk]::RequireDataDisk($folder)
    $image = [FyloTestDisk]::ImagePath($folder)
    if (-not (Test-Path -LiteralPath $image)) { [FyloTestDisk]::CreateImage($image, $sizeGiB) }
    [FyloTestDisk]::Prepare([FyloTestDisk]::AttachImage($image), $letter)
    [FyloTestDisk]::Publish($letter)
  }

  static [object] FindBySerial([string]$serial) {
    return @(Get-Disk | Where-Object { $_.SerialNumber -and $_.SerialNumber.Trim() -eq $serial })
  }

  static [object] FindUsb() {
    return @(Get-Disk | Where-Object { $_.BusType -eq 'USB' -and -not $_.IsBoot -and -not $_.IsSystem })
  }

  static [object] FindTestDisk([string]$serial) {
    $found = [FyloTestDisk]::FindBySerial($serial)
    if ($found.Count -eq 0) { $found = [FyloTestDisk]::FindUsb() }
    if ($found.Count -ne 1) { [FyloTestDisk]::Fail("found $($found.Count) disks with serial $serial (or USB disks); exactly one is expected") }
    return $found[0]
  }

  static [void] Online([string]$serial, [string]$letter) {
    $disk = [FyloTestDisk]::FindTestDisk($serial)
    [FyloTestDisk]::Say("test disk is disk $($disk.Number) ($($disk.BusType), $([math]::Round($disk.Size / 1GB, 1)) GiB, serial $($disk.SerialNumber))")
    [FyloTestDisk]::Prepare($disk, $letter)
    [FyloTestDisk]::Publish($letter)
  }

  static [void] Publish([string]$letter) {
    $drive = [FyloTestDisk]::Drive($letter)
    Write-Output "FYLO_TEST_DISK=$drive"
    if ($env:GITHUB_ENV) { Add-Content -LiteralPath $env:GITHUB_ENV -Value "FYLO_TEST_DISK=$drive" }
  }

  static [void] RequireWritable([string]$letter) {
    $probe = Join-Path ([FyloTestDisk]::Drive($letter)) ".fylo-write-probe-$PID"
    try {
      Set-Content -LiteralPath $probe -Value 'probe'
      Remove-Item -LiteralPath $probe -Force
    } catch {
      [FyloTestDisk]::Fail("$([FyloTestDisk]::Drive($letter)) is not writable: $($_.Exception.Message)")
    }
  }

  static [void] RequireSpace([string]$letter) {
    $volume = Get-Volume -DriveLetter $letter.TrimEnd(':')
    if ($volume.SizeRemaining -lt [FyloTestDisk]::MinimumFreeBytes) {
      [FyloTestDisk]::Fail("only $($volume.SizeRemaining) bytes free on $([FyloTestDisk]::Drive($letter)) (1 GiB needed)")
    }
  }

  static [void] RequireVolume([string]$letter) {
    $volume = Get-Volume -DriveLetter $letter.TrimEnd(':') -ErrorAction SilentlyContinue
    if (-not $volume) { [FyloTestDisk]::Fail("no volume has drive letter $letter") }
    if ($letter.TrimEnd(':') -eq $env:SystemDrive.TrimEnd(':')) { [FyloTestDisk]::Fail("$letter is the Windows drive") }
  }

  static [string] HostName() {
    return ($env:COMPUTERNAME -replace '[^A-Za-z0-9]', '')
  }

  static [bool] JobAlive([System.IO.DirectoryInfo]$folder) {
    $parts = $folder.Name.Split('-')
    $jobPid = 0
    $sameHost = $parts.Count -ge 3 -and $parts[-2] -eq [FyloTestDisk]::HostName()
    if (-not $sameHost) { return $folder.LastWriteTime -gt (Get-Date).AddMinutes(-[FyloTestDisk]::StaleMinutes) }
    if (-not [int]::TryParse($parts[-1], [ref]$jobPid)) { return $false }
    return [bool](Get-Process -Id $jobPid -ErrorAction SilentlyContinue)
  }

  static [void] Sweep([string]$roots) {
    if (-not (Test-Path -LiteralPath $roots)) { return }
    $limit = (Get-Date).AddMinutes(-[FyloTestDisk]::StaleMinutes)
    foreach ($folder in @(Get-ChildItem -LiteralPath $roots -Directory | Where-Object { $_.LastWriteTime -lt $limit })) {
      if ([FyloTestDisk]::JobAlive($folder)) { continue }
      [FyloTestDisk]::Say("removing the leftover job folder $($folder.FullName)")
      Remove-Item -LiteralPath $folder.FullName -Recurse -Force
    }
  }

  static [string] CleanOwner([string]$owner) {
    $clean = $owner -replace '[^A-Za-z0-9-]', '-'
    if (-not $clean) { return 'job' }
    return $clean
  }

  static [int] Run([string]$letter, [string]$owner, [string]$script, [string[]]$arguments) {
    if (-not (Test-Path -LiteralPath $script)) { [FyloTestDisk]::Fail("no script at $script") }
    [FyloTestDisk]::RequireVolume($letter)
    [FyloTestDisk]::RequireWritable($letter)
    [FyloTestDisk]::RequireSpace($letter)
    $roots = Join-Path ([FyloTestDisk]::Drive($letter)) 'roots'
    [FyloTestDisk]::Sweep($roots)
    $job = Join-Path $roots "$([FyloTestDisk]::CleanOwner($owner))-$([FyloTestDisk]::HostName())-$PID"
    New-Item -ItemType Directory -Force -Path $job | Out-Null
    [FyloTestDisk]::Say("FYLO_TEST_ROOT=$job")
    $env:FYLO_TEST_ROOT = $job
    try {
      $global:LASTEXITCODE = 0
      & $script @arguments | Out-Host
      return [FyloTestDisk]::ExitCode()
    } finally {
      Remove-Item -LiteralPath $job -Recurse -Force -ErrorAction SilentlyContinue
      [FyloTestDisk]::Say("removed $job")
    }
  }

  static [int] ExitCode() {
    if ($global:LASTEXITCODE) { return $global:LASTEXITCODE }
    return 0
  }

  static [void] Detach([string]$folder, [bool]$delete) {
    $image = [FyloTestDisk]::ImagePath($folder)
    if (-not (Test-Path -LiteralPath $image)) { [FyloTestDisk]::Say("no image at $image"); return }
    [FyloTestDisk]::Say("detaching $image")
    Dismount-DiskImage -ImagePath $image | Out-Null
    if (-not $delete) { return }
    [FyloTestDisk]::Say("deleting $image")
    Remove-Item -LiteralPath $image -Force
  }

  static [void] Status([string]$letter) {
    [FyloTestDisk]::RequireVolume($letter)
    Get-Volume -DriveLetter $letter.TrimEnd(':') | Format-Table DriveLetter, FileSystemLabel, FileSystem, Size, SizeRemaining | Out-Host
  }
}

function Get-FyloOwner([string]$given) {
  if ($given) { return $given }
  if ($env:FYLO_TEST_OWNER) { return $env:FYLO_TEST_OWNER }
  return 'job'
}

try {
  switch ($Action) {
    'Make' { [FyloTestDisk]::Make($Folder, $Letter, $SizeGiB) }
    'Online' { [FyloTestDisk]::Online($Serial, $Letter) }
    'Run' { exit [FyloTestDisk]::Run($Letter, (Get-FyloOwner $Owner), $Script, $Arguments) }
    'Detach' { [FyloTestDisk]::Detach($Folder, [bool]$Delete) }
    'Status' { [FyloTestDisk]::Status($Letter) }
  }
  exit 0
} catch {
  [Console]::Error.WriteLine($_.Exception.Message)
  exit 1
}
