# Fixtures

`objects.tsv` holds objects hashed by real git (SHA-1 and SHA-256 repositories): name, algorithm, kind, id, raw body in
hex. Git is a test oracle only: the tests read this file and never run git. To regenerate (needs `git` and `python3`),
run `./regenerate.sh` in this directory and commit the result.
