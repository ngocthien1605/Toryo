# Calculator module — fixture repository for the Thien Orchestrator M0 dry-run.
Set-StrictMode -Version Latest

function Add-Numbers {
    param([double]$A, [double]$B)
    return $A + $B
}

function Subtract-Numbers {
    param([double]$A, [double]$B)
    return $A - $B
}

function Multiply-Numbers {
    param([double]$A, [double]$B)
    return $A * $B
}

Export-ModuleMember -Function Add-Numbers, Subtract-Numbers, Multiply-Numbers
