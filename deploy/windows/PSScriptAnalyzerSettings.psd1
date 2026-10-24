# Settings for Invoke-ScriptAnalyzer in CI (job psscriptanalyzer).
@{
    Severity     = @('Error', 'Warning')
    ExcludeRules = @(
        # Interactive installers: they talk to the person at the console.
        'PSAvoidUsingWriteHost'
    )
}
