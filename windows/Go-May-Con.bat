@echo off
chcp 65001 >nul
title MinMin Remote - Go cai dat may con

net session >nul 2>&1
if %errorlevel% neq 0 (
  echo Can quyen Admin. Bam "Yes" o hop thoai hien ra...
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs" >nul 2>&1
  exit /b
)

setlocal
set "DIR=%LOCALAPPDATA%\MinMinRemote"
set "STARTUP=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"
set "TASK=MinMinRemote"
set "RUNKEY=HKCU\Software\Microsoft\Windows\CurrentVersion\Run"
set "WL=HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"

echo Dang go cai dat may con...
schtasks /end /tn "%TASK%" >nul 2>&1
schtasks /delete /tn "%TASK%" /f >nul 2>&1
reg delete "%RUNKEY%" /v "%TASK%" /f >nul 2>&1
del "%STARTUP%\MinMinRemote.vbs" >nul 2>&1
taskkill /IM mm-server.exe /F >nul 2>&1
rd /s /q "%DIR%" >nul 2>&1

rem Tat tu dang nhap Windows (tra ve binh thuong)
reg add "%WL%" /v AutoAdminLogon /t REG_SZ /d 0 /f >nul
reg delete "%WL%" /v DefaultPassword /f >nul 2>&1

echo.
echo Da go xong. May se KHONG tu chay va KHONG tu dang nhap nua.
pause
