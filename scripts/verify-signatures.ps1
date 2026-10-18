param(
    [Parameter(Mandatory = $true)][string[]]$Path,
    [Parameter(Mandatory = $true)][string]$ExpectedSubjectCommonName,
    # The values the program pins in its update signer check; a change here
    # needs the same change there, or installed copies refuse the release.
    [string]$ExpectedProfileEku = '1.3.6.1.4.1.311.97.230016310.8135122.214234108.684336535',
    [string]$ExpectedServiceEku = '1.3.6.1.4.1.311.97.1.0',
    [string]$ExpectedRootThumbprint = 'F40042E2E5F7E8EF8189FED15519AECE42C3BFA2'
)

# Fails unless every file carries a valid embedded Authenticode signature whose
# signer subject has the expected common name, with a timestamp
# countersignature, the expected key usages, and a chain to the expected root.
$ErrorActionPreference = 'Stop'
$failed = $false
foreach ($file in $Path) {
    $signature = Get-AuthenticodeSignature -FilePath $file
    $subject = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { '' }
    $expected = "CN=$ExpectedSubjectCommonName"
    $subjectOk = ($subject -split ',\s*') -contains $expected
    $stamped = $null -ne $signature.TimeStamperCertificate
    $usages = @()
    $root = ''
    if ($signature.SignerCertificate) {
        foreach ($extension in $signature.SignerCertificate.Extensions) {
            if ($extension -is [Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]) {
                $usages += $extension.EnhancedKeyUsages | ForEach-Object { $_.Value }
            }
        }
        $embedded = New-Object Security.Cryptography.X509Certificates.X509Certificate2Collection
        $embedded.Import($file)
        $chain = New-Object Security.Cryptography.X509Certificates.X509Chain
        $chain.ChainPolicy.RevocationMode = 'NoCheck'
        $chain.ChainPolicy.VerificationFlags = 'IgnoreNotTimeValid'
        $chain.ChainPolicy.ExtraStore.AddRange($embedded)
        if ($chain.Build($signature.SignerCertificate)) {
            $root = $chain.ChainElements[$chain.ChainElements.Count - 1].Certificate.Thumbprint
        }
    }
    $embeddedOk = $signature.SignatureType -eq 'Authenticode'
    $usageOk = ($usages -contains $ExpectedProfileEku) -and ($usages -contains $ExpectedServiceEku)
    $rootOk = $root -eq $ExpectedRootThumbprint
    if ($signature.Status -ne 'Valid' -or -not $subjectOk -or -not $stamped -or -not $embeddedOk -or -not $usageOk -or -not $rootOk) {
        Write-Host "FAIL $file status=$($signature.Status) type=$($signature.SignatureType) subject='$subject' timestamped=$stamped usages='$($usages -join ' ')' root=$root"
        $failed = $true
    } else {
        Write-Host "OK   $file"
    }
}
if ($failed) { throw 'One or more files are not correctly signed.' }
