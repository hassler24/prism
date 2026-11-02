param(
    [string]$Out = (Join-Path $PSScriptRoot "..\assets\prism.ico")
)

Add-Type -AssemblyName System.Drawing

function New-RoundedRectPath {
    param(
        [single]$X,
        [single]$Y,
        [single]$W,
        [single]$H,
        [single]$R
    )
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [single](2 * $R)
    $p.AddArc($X, $Y, $d, $d, 180, 90)
    $p.AddArc($X + $W - $d, $Y, $d, $d, 270, 90)
    $p.AddArc($X + $W - $d, $Y + $H - $d, $d, $d, 0, 90)
    $p.AddArc($X, $Y + $H - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

$sizes = 16, 24, 32, 48, 64, 128, 256
$frames = @()

foreach ($size in $sizes) {
    $ss = 4
    $big = $size * $ss

    $hi = New-Object System.Drawing.Bitmap($big, $big, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($hi)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $margin = [single](0.0625 * $big)
    $tile = New-RoundedRectPath $margin $margin ([single]($big - 2 * $margin)) ([single]($big - 2 * $margin)) ([single](0.21875 * $big))
    $top = [System.Drawing.Color]::FromArgb(255, 22, 24, 28)
    $bottom = [System.Drawing.Color]::FromArgb(255, 5, 6, 8)
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush(
        [System.Drawing.PointF]::new([single]0, [single]$margin),
        [System.Drawing.PointF]::new([single]0, [single]($big - $margin)),
        $top,
        $bottom
    )
    $g.FillPath($brush, $tile)

    $edgePen = New-Object System.Drawing.Pen(
        [System.Drawing.Color]::FromArgb(52, 255, 255, 255),
        [single](0.035 * $big)
    )
    $g.DrawPath($edgePen, $tile)

    $white = [System.Drawing.Color]::FromArgb(255, 250, 251, 253)
    $ringPen = New-Object System.Drawing.Pen($white, [single](0.065 * $big))
    $ringPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $ringPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $cx = [single](0.484375 * $big)
    $cy = [single](0.453125 * $big)
    $ringR = [single](0.1875 * $big)
    $g.DrawEllipse($ringPen, [single]($cx - $ringR), [single]($cy - $ringR), [single](2 * $ringR), [single](2 * $ringR))

    $handlePen = New-Object System.Drawing.Pen($white, [single](0.07 * $big))
    $handlePen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $handlePen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $g.DrawLine(
        $handlePen,
        [single](0.61875 * $big), [single](0.5875 * $big),
        [single](0.75625 * $big), [single](0.725 * $big)
    )

    $g.Dispose()
    $brush.Dispose()
    $tile.Dispose()
    $edgePen.Dispose()
    $ringPen.Dispose()
    $handlePen.Dispose()

    $small = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g2 = [System.Drawing.Graphics]::FromImage($small)
    $g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g2.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g2.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g2.DrawImage($hi, 0, 0, $size, $size)
    $g2.Dispose()
    $hi.Dispose()

    $ms = New-Object System.IO.MemoryStream
    $small.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $frames += ,@($size, $ms.ToArray())
    $small.Dispose()
    $ms.Dispose()
}

$dir = Split-Path -Parent $Out
if (-not (Test-Path -LiteralPath $dir)) {
    New-Item -ItemType Directory -Path $dir | Out-Null
}

$fs = [System.IO.File]::Create($Out)
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([uint16]0)
$bw.Write([uint16]1)
$bw.Write([uint16]$frames.Count)

$offset = 6 + 16 * $frames.Count
foreach ($f in $frames) {
    $size = [int]$f[0]
    $bytes = [byte[]]$f[1]
    $dim = if ($size -ge 256) { 0 } else { $size }
    $bw.Write([byte]$dim)
    $bw.Write([byte]$dim)
    $bw.Write([byte]0)
    $bw.Write([byte]0)
    $bw.Write([uint16]1)
    $bw.Write([uint16]32)
    $bw.Write([uint32]$bytes.Length)
    $bw.Write([uint32]$offset)
    $offset += $bytes.Length
}
foreach ($f in $frames) {
    $bw.Write([byte[]]$f[1])
}
$bw.Flush()
$bw.Dispose()
$fs.Dispose()

$png = [System.IO.Path]::ChangeExtension($Out, ".png")
$last = $frames[$frames.Count - 1]
[System.IO.File]::WriteAllBytes($png, [byte[]]$last[1])
Write-Output $Out
Write-Output $png
