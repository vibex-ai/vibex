@echo off
rem `vibex-tui` — the explicit name for the character-grid client.
rem
rem The client ships as `vibex.exe` beside this file. A batch launcher is what
rem Windows resolves from PATH for a second name, and `%~dp0` keeps it pointed
rem at the copy that was installed instead of whatever `vibex` PATH finds first.
"%~dp0vibex.exe" %*
