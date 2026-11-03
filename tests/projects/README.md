# Test projects

Projects for the test plugins. The plugin paths in them are relative to this
folder, so build the test plugins first:

```sh
scripts/build-test-plugins.sh
```

- `half-volume-*.refrain` have the volume saved at half. CI renders them and
  checks the plugins come back at half volume, then saves them again and
  checks nothing changed.
- `phrase-*.refrain` hold one clip: the demo phrase, recorded with
  `refrain record --demo`. CI renders them and checks they sound exactly like
  `refrain render --plugin <test plugin>`, which sends the same phrase as
  events.
- `mix-*.refrain` hold two tracks playing the phrase: the second through the
  test effect, 6 dB down and panned left, and the master through the test
  effect too. They were put together with `refrain new`, `track add`,
  `fx add` and `mix`. CI puts the same project together again, and checks it
  sounds exactly like these, in both formats.

```sh
cargo run -- render phrase.wav --plugin tests/projects/phrase-vst3.refrain
cargo run -- show tests/projects/mix-vst3.refrain
```
