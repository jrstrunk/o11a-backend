Set up an audit's source code with the needed documents (scope.txt, name.txt, documents.txt, security.md)
Run `forge build --ast` in the project's root to produce the needed files
Run `cargo run --bin o11a-backend`
To normalize documentation, run `cargo run --bin normalize_docs -- /home/john/audits/nudgexyz`

To mark a documentation file as technical, add a "technical: " prefix to the file path.
