# Test projects

Projects for the test plugins, with the volume saved at half. CI renders them
and checks that the plugins come back at half volume, then saves them again
and checks nothing changed. Build the test plugins first:

```sh
scripts/build-test-plugins.sh
cargo run -- render half.wav --plugin tests/projects/half-volume-vst3.refrain
```

The plugin paths are relative to this folder.
