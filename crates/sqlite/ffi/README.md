# Development flow for Mentat FFI

## Android SDK
Android Mentat SDK is one of two first-party consumers (the other one being iOS Mentat SDK).

Binaries produced by `cargo build ...` are symlinked from within the SDK, and a number of target-specific `libmentat_ffi.so` binaries are bundled together and distributed with the SDK.

There is a build script at `<mentat_root>/scripts/android_build.sh` which knows how to cross-compile `mentat_ffi` for various Android targets.

- `./<mentat_root>/scripts/android_build.sh` - compiles for all supported targets
- `./<mentat_root>/scripts/android_build.sh x86` - compiles for `x86`
- `./<mentat_root>/scripts/android_build.sh x86 arm` - compiles for `x86`, `arm` 

General development flow while working on the Android SDK is:
- (pre) compile for all targets, if you've never done so
- make changes to `mentat_ffi` and/or `mentat`
- re-compile `mentat_ffi` binaries using `./android_build.sh x86`. During development it's faster to compile just for the target which matches your emulator, e.g. `x86`
- make corresponding changes in the Android SDK, try them out from within the bundled sample project
- since binaries are symlinked, no manual copy step is necessary for the Android SDK to pick up the changes

## iOS SDK
TODO, contribute via [issue #732](https://github.com/mozilla/mentat/issues/732).

## Panics and threads

No panic unwinds into C. Most `extern "C"` functions catch panics: a panic
is reported through the `ExternError` out parameter where the function has
one (the message starts with `mentat panicked: `), and the function returns
a null pointer, 0, 0.0 or nothing.

The getters that return a plain number and have no `ExternError` abort the
process on a panic, as before: 0 is a valid long, boolean, double, timestamp
and entid, so returning it would hide the error (a misspelled attribute name
would quietly become entid 0). They are
`typed_value_into_{long,entid,boolean,double,timestamp}`,
`value_at_index_into_{long,entid,boolean,double,timestamp}`,
`store_entid_for_attribute` (panics on an attribute not in the schema),
`tx_report_get_entid`, `tx_report_get_tx_instant`, `changelist_entry_at` and
`typed_value_value_type`. Check the value type, the index or the attribute
before calling them.

A `Store` handle, and every handle made from it (`InProgress`, the builders,
`QueryBuilder`, result sets, iterators), must not be used from two threads at
once. The SQLite connection and the store's caches are not synchronised. If
threads share a store, serialise every call on it with your own lock, or open
one `Store` per thread on the same file. Transaction observer callbacks run on
a thread owned by Mentat and must not call back into the same `Store`.
