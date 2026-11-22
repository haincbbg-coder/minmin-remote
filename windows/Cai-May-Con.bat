@echo off
chcp 65001 >nul
title MinMin Remote - Cai dat may con
setlocal

set "DIR=%LOCALAPPDATA%\MinMinRemote"
set "STARTUP=%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup"
set "RELAY=187.77.157.250:5000"
set "TASK=MinMinRemote"

echo ============================================================
echo    MINMIN REMOTE  -  CAI DAT MAY CON
echo    Chay AN, tu noi lai khi mat mang, tu chay khi mo may.
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

rem --- Dung & xoa ban cu (ca co lan tac vu) ---
schtasks /end /tn "%TASK%" >nul 2>&1
schtasks /delete /tn "%TASK%" /f >nul 2>&1
del "%STARTUP%\MinMinRemote.vbs" >nul 2>&1
taskkill /IM mm-server.exe /F >nul 2>&1

rem --- Chep exe vao thu muc co dinh ---
if not exist "%DIR%" mkdir "%DIR%"
copy /Y "%~dp0mm-server.exe" "%DIR%\mm-server.exe" >nul
if not exist "%DIR%\mm-server.exe" (
  echo LOI: khong thay mm-server.exe canh file nay.
  echo Hay de Cai-May-Con.bat cung thu muc voi mm-server.exe.
  pause
  exit /b 1
)

rem --- Tao Tac vu he thong: chay khi DANG NHAP Windows (an, vi exe la app nen) ---
schtasks /create /tn "%TASK%" /sc onlogon /f /tr "\"%DIR%\mm-server.exe\" --relay %RELAY% --device-id %COMPUTERNAME% --device-name %COMPUTERNAME% --password %MM_PW%" >nul
if errorlevel 1 (
  echo CANH BAO: tao tac vu that bai. Thu chay lai file nay bang quyen Admin.
)

rem --- Chay ngay bay gio (an) ---
start "" "%DIR%\mm-server.exe" --relay %RELAY% --device-id %COMPUTERNAME% --device-name %COMPUTERNAME% --password %MM_PW%

echo.
echo ============================================================
echo    DA CAI XONG!
echo    May "%COMPUTERNAME%" dang chay nen (an).
echo    - Tu NOI LAI khi mat mang / relay khoi dong lai.
echo    - Tu chay moi khi DANG NHAP Windows.
echo    Muon go bo: chay  Go-May-Con.bat
echo ============================================================
echo.
pause
