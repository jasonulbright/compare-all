param(
    [Parameter(Mandatory = $true)][string[]]$Path,
    [Parameter(Mandatory = $true)][string]$ExpectedSubjectCommonName
)

# Fails unless every file carries a valid Authenticode signature whose signer
# subject has the expected common name and a timestamp countersignature.
$ErrorActionPreference = 'Stop'
$failed = $false
foreach ($file in $Path) {
    $signature = Get-AuthenticodeSignature -FilePath $file
    $subject = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { '' }
    $expected = "CN=$ExpectedSubjectCommonName"
    $subjectOk = ($subject -split ',\s*') -contains $expected
    $stamped = $null -ne $signature.TimeStamperCertificate
    if ($signature.Status -ne 'Valid' -or -not $subjectOk -or -not $stamped) {
        Write-Host "FAIL $file status=$($signature.Status) subject='$subject' timestamped=$stamped"
        $failed = $true
    } else {
        Write-Host "OK   $file"
    }
}
if ($failed) { throw 'One or more files are not correctly signed.' }
