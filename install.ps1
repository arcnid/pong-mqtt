# MQTT Pong Installer for Windows
Write-Host "🏓 MQTT Pong Installer" -ForegroundColor Cyan
Write-Host "=====================" -ForegroundColor Cyan
Write-Host ""

# Check if cargo is installed
$cargoInstalled = Get-Command cargo -ErrorAction SilentlyContinue

if ($cargoInstalled) {
    Write-Host "✅ Rust is already installed ($((cargo --version)))" -ForegroundColor Green
} else {
    Write-Host "❌ Rust not found. Installing..." -ForegroundColor Yellow
    Write-Host ""

    # Check if Chocolatey is available
    $chocoInstalled = Get-Command choco -ErrorAction SilentlyContinue

    if ($chocoInstalled) {
        Write-Host "🍫 Installing Rust via Chocolatey..." -ForegroundColor Cyan
        choco install rust -y

        # Refresh environment variables
        $env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")
    } else {
        Write-Host "⚠️  Chocolatey not found." -ForegroundColor Yellow
        Write-Host ""
        Write-Host "Please install Rust manually:" -ForegroundColor Yellow
        Write-Host "  1. Visit https://rustup.rs/" -ForegroundColor White
        Write-Host "  2. Download and run rustup-init.exe" -ForegroundColor White
        Write-Host "  3. Follow the installer prompts" -ForegroundColor White
        Write-Host "  4. Restart this PowerShell window" -ForegroundColor White
        Write-Host "  5. Run this script again" -ForegroundColor White
        Write-Host ""
        Write-Host "Or install Chocolatey first:" -ForegroundColor Yellow
        Write-Host "  Visit https://chocolatey.org/install" -ForegroundColor White
        exit 1
    }

    Write-Host ""
    Write-Host "✅ Rust installed successfully!" -ForegroundColor Green
}

Write-Host ""
Write-Host "🔨 Building MQTT Pong..." -ForegroundColor Cyan
Write-Host ""

# Build the game in release mode
cargo build --release

if ($LASTEXITCODE -eq 0) {
    Write-Host ""
    Write-Host "✅ Build complete!" -ForegroundColor Green
    Write-Host ""
    Write-Host "🎮 To play, run:" -ForegroundColor Cyan
    Write-Host "   cargo run --release" -ForegroundColor White
    Write-Host ""
    Write-Host "   Or use the binary directly:" -ForegroundColor Cyan
    Write-Host "   .\target\release\rust-pong.exe" -ForegroundColor White
    Write-Host ""
    Write-Host "📖 For more info, see README.md" -ForegroundColor Cyan
    Write-Host ""
} else {
    Write-Host ""
    Write-Host "❌ Build failed. Please check the error messages above." -ForegroundColor Red
    exit 1
}
