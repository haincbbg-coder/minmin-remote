@echo off
chcp 65001 >nul
title MinMin Remote - Cai dat may con
setlocal

set "DIR=%LOCALAPPDATA%\MinMinRemote"
set "STARTUP=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"
set "RELAY=187.77.157.250:5000"

echo ============================================================
echo    MINMIN REMOTE  -  CAI DAT MAY CON
echo    Sau khi cai, may nay TU CHAY AN moi khi mo may.
echo    Khong can mo cua so, khong can lam gi them.
echo ============================================================
echo.
set "MM_PW="
set /p MM_PW=Nhap mat khau dieu khien (phai giong ben may xem):
if "%MM_PW%"=="" (
  echo.
  echo Chua nhap mat khau. Thoat.
  pause
  exit /b 1
)

echo.
echo Dang cai...

rem Dung ban cu neu dang chay
taskkill /IM mm-server.exe /F >nul 2>&1

rem Chep mm-server.exe vao thu muc co dinh
if not exist "%DIR%" mkdir "%DIR%"
copy /Y "%~dp0mm-server.exe" "%DIR%\mm-server.exe" >nul
if not exist "%DIR%\mm-server.exe" (
  echo LOI: khong tim thay mm-server.exe canh file nay.
  echo Hay de Cai-May-Con.bat cung thu muc voi mm-server.exe.
  pause
  exit /b 1
)

rem Tao file chay AN, dat vao thu muc Startup -> tu chay khi dang nhap Windows
> "%STARTUP%\MinMinRemote.vbs" echo CreateObject("WScript.Shell").Run """%DIR%\mm-server.exe"" --relay %RELAY% --device-id %COMPUTERNAME% --device-name ""%COMPUTERNAME%"" --password %MM_PW%", 0, False

rem Chay ngay bay gio (an)
wscript "%STARTUP%\MinMinRemote.vbs"

echo.
echo ============================================================
echo    DA CAI XONG!
echo    May "%COMPUTERNAME%" dang chay nen (an).
echo    Tu gio moi khi mo may se tu chay - khong can lam gi.
echo.
echo    Muon go bo: chay file  Go-May-Con.bat
echo ============================================================
echo.
pause
