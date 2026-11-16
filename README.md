# Tasks — a SimpleEdit plugin

The build, test and run commands of a project (Cargo, npm scripts, Make, Go, and `.simpleedit/tasks.json`: `[{"name", "command", "cwd"}]`), run from a panel of [SimpleEdit](https://github.com/simple-edit-app/simpleedit) with their output. A place the output names (`src/a.rs:12:5`) opens the file there; errors and warnings show in their files. The commands a project writes itself run once you allow it.

## Build

```sh
cargo test
cargo build --release && mkdir -p bin && cp target/release/tasks bin/
```

## License

MIT
