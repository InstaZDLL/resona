param(
    [Parameter(Mandatory = $true)]
    [string]$Path
)

$ErrorActionPreference = 'Stop'

function Read-Exactly([IO.Stream]$Stream, [int]$Count) {
    $bytes = [byte[]]::new($Count)
    $Stream.ReadExactly($bytes)
    return ,$bytes
}

function Write-BigEndian([IO.Stream]$Stream, [UInt64]$Value, [int]$Count) {
    for ($i = $Count - 1; $i -ge 0; $i--) {
        $Stream.WriteByte([byte](($Value -shr ($i * 8)) -band 255))
    }
}

function Repair-Flac([IO.FileInfo]$File) {
    $input = [IO.File]::OpenRead($File.FullName)
    try {
        if ([Text.Encoding]::ASCII.GetString((Read-Exactly $input 4)) -ne 'fLaC') {
            throw "Not a native FLAC file: $($File.FullName)"
        }
        $sampleRate = 0
        $lastHeaderOffset = 0L
        do {
            $lastHeaderOffset = $input.Position
            $header = Read-Exactly $input 4
            $last = ($header[0] -band 128) -ne 0
            $type = $header[0] -band 127
            $length = ([int]$header[1] -shl 16) -bor ([int]$header[2] -shl 8) -bor [int]$header[3]
            if ($type -eq 3) {
                Write-Output "Already indexed: $($File.Name)"
                return
            }
            if ($type -eq 0) {
                if ($length -ne 34) { throw 'Invalid STREAMINFO block' }
                $info = Read-Exactly $input $length
                $sampleRate = ([int]$info[10] -shl 12) -bor ([int]$info[11] -shl 4) -bor ([int]$info[12] -shr 4)
            } else {
                $null = $input.Seek($length, [IO.SeekOrigin]::Current)
            }
        } until ($last)
        $audioStart = $input.Position
        if ($sampleRate -le 0) { throw 'Invalid sample rate' }

        $packets = & ffprobe -v error -select_streams a:0 -show_packets -show_entries packet=pts,duration,pos -of compact=p=0:nk=0 $File.FullName
        if ($LASTEXITCODE -ne 0) { throw 'ffprobe could not read the audio frames' }
        $points = [IO.MemoryStream]::new()
        try {
            [UInt64]$next = 0
            [UInt64]$previous = 0
            foreach ($line in $packets) {
                $values = @{}
                foreach ($field in ($line -split '\|')) {
                    $pair = $field -split '=', 2
                    if ($pair.Count -eq 2) { $values[$pair[0]] = $pair[1] }
                }
                if (-not ($values.ContainsKey('pts') -and $values.ContainsKey('pos') -and $values.ContainsKey('duration'))) {
                    throw "Missing frame information: $line"
                }
                [UInt64]$sample = $values.pts
                [UInt64]$position = $values.pos
                [UInt64]$duration = $values.duration
                if ($position -lt [UInt64]$audioStart -or $duration -gt 65535 -or $sample -lt $previous) {
                    throw "Invalid frame information: $line"
                }
                if ($sample -ge $next) {
                    Write-BigEndian $points $sample 8
                    Write-BigEndian $points ($position - [UInt64]$audioStart) 8
                    Write-BigEndian $points $duration 2
                    $next = $sample + [UInt64]$sampleRate * 10
                }
                $previous = $sample
            }
            if ($points.Length -eq 0 -or $points.Length -gt 0xFFFFFF) { throw 'Invalid seek table size' }
            $payload = $points.ToArray()
        } finally {
            $points.Dispose()
        }

        $temporary = Join-Path $File.DirectoryName ('.' + $File.Name + '.seekfix-' + [guid]::NewGuid().ToString('N') + '.tmp')
        try {
            $output = [IO.File]::Create($temporary)
            try {
                $null = $input.Seek(0, [IO.SeekOrigin]::Begin)
                $metadata = Read-Exactly $input ([int]$audioStart)
                $metadata[$lastHeaderOffset] = $metadata[$lastHeaderOffset] -band 127
                $output.Write($metadata)
                $output.WriteByte(0x83)
                Write-BigEndian $output ([UInt64]$payload.Length) 3
                $output.Write($payload)
                $input.CopyTo($output)
            } finally {
                $output.Dispose()
            }
            $expected = $File.Length + 4 + $payload.Length
            if ((Get-Item -LiteralPath $temporary).Length -ne $expected) { throw 'Copied file size mismatch' }
            $probe = & ffprobe -v error -select_streams a:0 -show_entries stream=sample_rate,duration -of csv=p=0 $temporary
            if ($LASTEXITCODE -ne 0 -or -not $probe) { throw 'Repaired FLAC failed validation' }
            $input.Dispose()
            [IO.File]::Move($temporary, $File.FullName, $true)
            Write-Output "Indexed: $($File.Name) ($($payload.Length / 18) points)"
        } finally {
            if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary }
        }
    } finally {
        $input.Dispose()
    }
}

$item = Get-Item -LiteralPath $Path
$files = if ($item.PSIsContainer) {
    Get-ChildItem -LiteralPath $item.FullName -Filter '*.flac' -File -Recurse
} else {
    @($item)
}
foreach ($file in $files) { Repair-Flac $file }
