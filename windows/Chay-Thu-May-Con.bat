@echo off
chcp 65001 >nul
title MinMin Remote - May con (chay thu, co cua so de xem log)
setlocal

echo Chay thu mm-server (hien cua so + log). Dong cua so la tat.
echo De cai tu dong chay nen, dung  Cai-May-Con.bat  thay vi file nay.
echo.
set "MM_PW="
set /p MM_PW=Nhap mat khau dieu khien:
if "%MM_PW%"=="" (echo Chua nhap mat khau.& pause & exit /b 1)

"%~dp0mm-server.exe" --relay 187.77.157.250:5000 --device-id %COMPUTERNAME% --device-name "%COMPUTERNAME%" --password %MM_PW%
echo.
echo (mm-server da dung)
pause
