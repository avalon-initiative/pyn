# Fixtures

`objects.tsv` holds objects hashed by real git (SHA-1 and SHA-256 repositories): name, algorithm, kind, id, raw body in
hex. Git is a test oracle only: the tests read this file and never run git. To regenerate (needs `git` and `python3`),
run `./regenerate.sh` in this directory and commit the result.

`sha-mbles-1.bin` and `sha-mbles-2.bin` (640 bytes each) are the public SHA-1 chosen-prefix collision pair (SHA-mbles,
Leurent and Peyrin), copied from the test data of the `sha1-checked` crate (MIT/Apache-2.0). Both hash to SHA-1
`8ac60ba76f1999a1ab70223f225aefdc78d4ddc0`; they are used as opaque bytes only.
