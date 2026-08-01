# 希尔娅 本地视觉 OCR（Windows）
# 用法: powershell -NoProfile -ExecutionPolicy Bypass -File vision-ocr.ps1 <图片路径>
# 输出: JSON { "ocr": ["第1行文字", "第2行文字", ...] }
# 依赖: 系统需安装 OCR 语言包（设置 -> 时间和语言 -> 语言 -> 中文 -> 选项 -> 光学字符识别）

param([Parameter(Mandatory = $true)][string]$ImagePath)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $ImagePath)) {
    Write-Output '{"ocr": []}'
    exit 0
}

try {
    # 加载 WinRT 类型
    [void][System.Runtime.WindowsRuntime, System.Runtime, ContentType = WindowsRuntime]
    [void][Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType = WindowsRuntime]
    [void][Windows.Graphics.Imaging.BitmapDecoder, Windows.Foundation, ContentType = WindowsRuntime]
    [void][Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime]
    [void][Windows.Globalization.Language, Windows.Globalization, ContentType = WindowsRuntime]

    # 通用的 WinRT IAsyncOperation -> .NET Task 等待器
    $asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() |
        Where-Object {
            $_.Name -eq 'AsTask' -and
            $_.GetParameters().Count -eq 1 -and
            $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
        })[0]

    function Await($WinRtTask, $ResultType) {
        $asTask = $asTaskGeneric.MakeGenericMethod($ResultType)
        $netTask = $asTask.Invoke($null, @($WinRtTask))
        $netTask.Wait(-1) | Out-Null
        $netTask.Result
    }

    $fullPath = (Resolve-Path -LiteralPath $ImagePath).Path
    $file = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync($fullPath)) ([Windows.Storage.StorageFile])
    $stream = Await ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
    $decoder = Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
    $bitmap = Await ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])

    # 优先中文，失败回退用户配置语言
    $engine = $null
    try {
        $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromLanguage((New-Object Windows.Globalization.Language('zh-Hans')))
    } catch {
        $engine = $null
    }
    if (-not $engine) {
        $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
    }
    if (-not $engine) {
        Write-Error '未找到可用的 OCR 语言包（请安装：设置 -> 时间和语言 -> 语言 -> 中文 -> 选项 -> 光学字符识别）'
        exit 1
    }

    $result = Await ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])
    $lines = @($result.Lines | ForEach-Object { $_.Text } | Where-Object { $_ -ne '' })
    if ($lines.Count -eq 0) { $lines = @() }

    Write-Output (ConvertTo-Json -InputObject @{ ocr = $lines } -Compress)
} catch {
    Write-Error $_.Exception.Message
    exit 1
}
