# alienstro/homebrew-tap

Homebrew formulae for [xmusic](https://github.com/alienstro/xMusic).

## Install

```bash
brew install alienstro/tap/xmusic
```

One command — Homebrew taps this repository automatically when you use the
fully-qualified name. Afterwards the short name works everywhere:

```bash
brew upgrade xmusic
brew info xmusic
brew uninstall xmusic
```

## Why this repository exists separately

Homebrew only auto-taps repositories named `homebrew-<something>`, which is why
this is its own repo rather than living inside xMusic. It holds one file, and
only changes when xmusic cuts a release.

The formula's source of truth is `packaging/homebrew/xmusic.rb` in the xMusic
repository; this is a copy.
