Assert-Equal 'safe: slash and colon' 'a_b_c' (ConvertTo-SafeFileName 'a/b:c')
Assert-Equal 'safe: trims trailing dot and space' 'report' (ConvertTo-SafeFileName ' report. ')
Assert-Equal 'safe: unchanged' 'hello.txt' (ConvertTo-SafeFileName 'hello.txt')
