@echo off
chcp 65001 >nul
title MinMin Remote - Go cai dat may con
setlocal

set "DIR=%LOCALAPPDATA%\MinMinRemote"
set "STARTUP=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"

echo Dang go cai dat may con...
taskkill /IM mm-server.exe /F >nul 2>&1
del "%STARTUP%\MinMinRemote.vbs" >nul 2>&1
rd /s /q "%DIR%" >nul 2>&1

echo.
echo Da go xong. May nay se KHONG tu chay nua.
pause
