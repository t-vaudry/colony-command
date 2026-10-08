# No settings.json existed on Windows before the capture hook, so remove it,
# but only if it still contains nothing except the capture hooks.
$s = "$env:USERPROFILE\.claude\settings.json"
$ours = Get-Content (Join-Path $PSScriptRoot 'hooks-capture.json') -Raw
if ((Get-Content $s -Raw) -eq $ours) { Remove-Item $s; "windows: removed capture settings" }
else { "windows: settings.json was edited since install; remove the 'hooks' block by hand" }
