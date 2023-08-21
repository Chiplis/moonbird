__Update: Twitter has disabled the default bearer token used by Moonbird, which means the app will not work unless you provide it your own bearer token.__

# moonbird
A small utility that lets you download a copy of a previously published Twitter Space for local playback.

## Building

```cargo build --release```

## Usage
Arguments between curly braces are mandatory, those between brackets are optional and will fallback to default values.

```./moonbird -s {SpaceID} -b [BearerToken] -p [AudioFileName] -c [MaxConcurrentFragmentDownloads]```
