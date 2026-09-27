# BatchRename — small PowerShell module used to learn the thien workflow.
Set-StrictMode -Version Latest

function ConvertTo-SafeFileName {
    <#
    .SYNOPSIS
      Replace characters that are invalid in Windows file names with '_' and trim spaces/dots.
    #>
    [CmdletBinding()]
    param([Parameter(Mandatory, ValueFromPipeline)][string]$Name)
    process {
        $invalid = [System.IO.Path]::GetInvalidFileNameChars() + [char[]]'<>:"/\|?*'
        $chars = $Name.ToCharArray() | ForEach-Object { if ($invalid -contains $_) { '_' } else { $_ } }
        (-join $chars).Trim().TrimEnd('.')
    }
}

Export-ModuleMember -Function ConvertTo-SafeFileName
