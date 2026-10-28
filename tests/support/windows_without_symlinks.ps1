function Invoke-WithoutDeveloperSymlinks {
    param([Parameter(Mandatory)][scriptblock]$Body)

    $policyPath = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock'
    $policyName = 'AllowDevelopmentWithoutDevLicense'
    $key = Get-Item -LiteralPath $policyPath -ErrorAction SilentlyContinue
    $hadValue = $null -ne $key -and $key.GetValueNames() -contains $policyName
    $oldValue = if ($hadValue) { [int]$key.GetValue($policyName) } else { $null }
    $changed = $false
    Write-Output "Developer symlink allowance before comparison: $oldValue"
    try {
        if ($hadValue -and $oldValue -ne 0) {
            # This dedicated CI VM is the test fixture. Never grant a privilege,
            # create a policy, or leave the runner's original value changed.
            $changed = $true
            Set-ItemProperty -LiteralPath $policyPath -Name $policyName -Value 0 -ErrorAction Stop
            if ((Get-ItemPropertyValue -LiteralPath $policyPath -Name $policyName) -ne 0) {
                throw 'Could not establish the native no-symlink test condition'
            }
        }
        & $Body
    }
    finally {
        if ($changed) {
            Set-ItemProperty -LiteralPath $policyPath -Name $policyName -Value $oldValue -ErrorAction Stop
            if ((Get-ItemPropertyValue -LiteralPath $policyPath -Name $policyName) -ne $oldValue) {
                throw 'Failed to restore the runner developer symlink allowance'
            }
        }
        Write-Output "Developer symlink allowance restored: $oldValue"
    }
}
