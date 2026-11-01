@echo off
chcp 936 >nul
setlocal enabledelayedexpansion

REM ============================================================
REM  ONYX 反破解体系 · M0 构建脚本（控制端 / Flutter UI）
REM  仅操作员本机使用，不随客户端分发
REM ============================================================

set ROOT=%~dp0

echo [1/3] 编译控制端 (release, x64, --features flutter)...
pushd "%ROOT%"
cargo build --release --features flutter
set BUILD_RC=%ERRORLEVEL%
popd

if not "%BUILD_RC%"=="0" (
    echo [ERROR] cargo build 失败，退出码 %BUILD_RC%
    exit /b %BUILD_RC%
)

set EXE=%ROOT%target\release\rustdesk.exe
if not exist "%EXE%" (
    echo [ERROR] 未找到产物 %EXE%
    exit /b 1
)

echo [2/3] 产物信息:
for %%A in ("%EXE%") do echo   大小: %%~zA 字节  时间: %%~tA

echo [3/3] 计算哈希...
certutil -hashfile "%EXE%" SHA256 | findstr /r /v "^CertUtil"

echo.
echo ============================================================
echo [OK] 控制端构建完成: %EXE%
echo ============================================================
endlocal
