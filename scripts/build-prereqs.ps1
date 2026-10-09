# Dot-sourced by update.ps1 and build-installer.ps1. Dictation builds whisper.cpp,
# which needs CMake, and generates its bindings with bindgen, which needs libclang.
# Finds them in their default install folders and fails early, not deep in cargo.
$cmakeBin = "C:\Program Files\CMake\bin"
$llvmBin = "C:\Program Files\LLVM\bin"
if (Test-Path $cmakeBin) { $env:PATH = "$cmakeBin;$env:PATH" }
if (-not $env:LIBCLANG_PATH -and (Test-Path (Join-Path $llvmBin "libclang.dll"))) { $env:LIBCLANG_PATH = $llvmBin }
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    throw "CMake is required to build the speech engine and wasn't found. Install it with: winget install Kitware.CMake"
}
if (-not $env:LIBCLANG_PATH -or -not (Test-Path (Join-Path $env:LIBCLANG_PATH "libclang.dll"))) {
    throw "LLVM (libclang) is required to build the speech engine and wasn't found. Install it with: winget install LLVM.LLVM, or set LIBCLANG_PATH to the folder holding libclang.dll."
}
