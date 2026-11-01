@echo off
chcp 936 >nul
setlocal enabledelayedexpansion

REM ============================================================
REM  ONYX 反破解体系 · M0 构建脚本（headless agent）
REM  产物：rustdesk.exe（无 UI，服务模式）→ 复制为 OnyxAssist.exe
REM  注意：不要加 --features flutter，否则会带出 Flutter UI
REM ============================================================

set ROOT=%~dp0
set LAUNCHER=%~dp0..\MyProjcet\OnyxLauncher

echo [1/4] 编译 headless agent (release, x64)...
pushd "%ROOT%"
cargo build --release
set BUILD_RC=%ERRORLEVEL%
popd

if not "%BUILD_RC%"=="0" (
    echo [ERROR] cargo build 失败，退出码 %BUILD_RC%
    exit /b %BUILD_RC%
)

set AGENT=%ROOT%target\release\rustdesk.exe
if not exist "%AGENT%" (
    echo [ERROR] 未找到产物 %AGENT%
    exit /b 1
)

echo [2/4] 自检 --print-id...
"%AGENT%" --print-id
if not "%ERRORLEVEL%"=="0" (
    echo [ERROR] --print-id 执行失败
    exit /b 1
)

echo [3/4] 复制到启动器目录...
if not exist "%LAUNCHER%" (
    echo [ERROR] 启动器目录不存在: %LAUNCHER%
    exit /b 1
)
copy /Y "%AGENT%" "%LAUNCHER%\OnyxAssist.exe" >nul
if not "%ERRORLEVEL%"=="0" (
    echo [ERROR] 复制失败
    exit /b 1
)

echo [4/4] 计算哈希...
certutil -hashfile "%LAUNCHER%\OnyxAssist.exe" SHA256 | findstr /r /v "^CertUtil"

echo.
echo ============================================================
echo [OK] agent 构建完成
echo   产物: %LAUNCHER%\OnyxAssist.exe
echo   下一步: 运行 %LAUNCHER%\build_all.bat 完成加密 + 启动器 + 加壳
echo ============================================================
endlocal
