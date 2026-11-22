@echo off
chcp 65001 >nul
title MinMin Remote - Cai dat may con

rem ===== Tu xin quyen Admin =====
net session >nul 2>&1
if %errorlevel% neq 0 (
  echo Can quyen Admin. Bam "Yes" o hop thoai hien ra...
  powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs" >nul 2>&1
  exit /b
)

setlocal EnableExtensions
set "DIR=%LOCALAPPDATA%\MinMinRemote"
set "STARTUP=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"
set "RELAY=187.77.157.250:5000"
set "TASK=MinMinRemote"
set "RUNKEY=HKCU\Software\Microsoft\Windows\CurrentVersion\Run"
set "WL=HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"

echo ============================================================
echo    MINMIN REMOTE  -  CAI MAY CON (cai 1 lan la xong)
echo    Reset may -^> tu dang nhap -^> tu chay -^> online.
echo    Mat mang cung tu noi lai. Khong can lam gi them.
echo ============================================================
echo.
set "MM_PW="
set /p MM_PW=1) Mat khau DIEU KHIEN (giong ben may xem):
if "%MM_PW%"=="" (echo Chua nhap mat khau dieu khien.& pause & exit /b 1)
echo.
echo 2) Mat khau DANG NHAP WINDOWS cua may nay
echo    (de may tu dang nhap sau reset; khong co mat khau thi de trong, Enter).
set "WIN_PW="
set /p WIN_PW=   Mat khau Windows:

echo.
echo Dang cai...

rem --- Don ban cu moi loai ---
schtasks /end /tn "%TASK%" >nul 2>&1
schtasks /delete /tn "%TASK%" /f >nul 2>&1
reg delete "%RUNKEY%" /v "%TASK%" /f >nul 2>&1
del "%STARTUP%\MinMinRemote.vbs" >nul 2>&1
taskkill /IM mm-server.exe /F >nul 2>&1

rem --- Chep exe vao thu muc co dinh ---
if not exist "%DIR%" mkdir "%DIR%"
copy /Y "%~dp0mm-server.exe" "%DIR%\mm-server.exe" >nul
if not exist "%DIR%\mm-server.exe" (
  echo LOI: khong thay mm-server.exe canh file nay.
  pause
  exit /b 1
)

rem --- FILE KHOI DONG DUY NHAT: start.vbs (chay mm-server AN) ---
> "%DIR%\start.vbs" echo CreateObject("WScript.Shell").Run """%DIR%\mm-server.exe"" --relay %RELAY% --device-id %COMPUTERNAME% --device-name %COMPUTERNAME% --password %MM_PW%", 0, False

rem --- Bat TU DANG NHAP Windows ---
reg add "%WL%" /v AutoAdminLogon /t REG_SZ /d 1 /f >nul
reg add "%WL%" /v DefaultUserName /t REG_SZ /d "%USERNAME%" /f >nul
reg add "%WL%" /v DefaultDomainName /t REG_SZ /d "%USERDOMAIN%" /f >nul
reg add "%WL%" /v DefaultPassword /t REG_SZ /d "%WIN_PW%" /f >nul
reg add "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\PasswordLess\Device" /v DevicePasswordLessBuildVersion /t REG_DWORD /d 0 /f >nul 2>&1

rem --- Dang ky TU CHAY: 2 cach cung tro vao start.vbs ---
reg add "%RUNKEY%" /v "%TASK%" /t REG_SZ /d "wscript.exe \"%DIR%\start.vbs\"" /f >nul
schtasks /create /tn "%TASK%" /sc onlogon /rl highest /f /tr "wscript.exe \"%DIR%\start.vbs\"" >nul

rem --- Chay NGAY bang chinh file khoi dong do (giong het luc tu chay) ---
wscript.exe "%DIR%\start.vbs"

echo.
echo ============================================================
echo    DA CAI XONG!  May "%COMPUTERNAME%".
echo    - Dang chay nen ngay bay gio.
echo    - Reset / dang nhap lai -^> tu chay (2 cach bao dam).
echo    - Mat mang -^> tu noi lai.
echo    Go bo: chay  Go-May-Con.bat
echo ============================================================
echo.
echo Neu may xem van khong thay online sau 30 giay, mo file nay gui con:
echo    %DIR%\mm-server.log
echo.
pause
