# Test fixture -- a synthetic ADS-B CSV sample (question 200(a), the edge track's second plugin)

`sample.csv` is a small, **synthetic**, hand-generated ADS-B position sample -- not a live
receiver capture, not a recorded feed, no network access at any point (question 154). It
exists so `crates/av-edge/tests/plugin_replay_adsb.rs`,
`crates/av-kernel/tests/edge_plugin_adsb_ecef_crosscheck.rs` and `crates/av-edge/src/
plugin/adsb.rs`'s own unit tests are offline and deterministic.

## What the columns model

```
icao24,utc_unix_s,lat_deg,lon_deg,geo_alt_m
```

- `icao24` -- the transponder's 24-bit ICAO address, six hex digits.
- `utc_unix_s` -- the receiver's UTC reception timestamp, integer Unix-epoch seconds.
- `lat_deg`/`lon_deg` -- geodetic WGS-84 latitude/longitude, degrees, already CPR-decoded
  (a real DF17 message's raw even/odd CPR frames are not modeled -- see `crate::plugin::
  adsb`'s own module doc for the full list of what a real ADS-B stream carries that this
  fixture deliberately leaves out: identification/velocity messages, NIC/NACp/SIL fields,
  squawk, surveillance status, capability flags, DF11 all-call, Mode S CRC).
- `geo_alt_m` -- **geometric** altitude (height above the WGS-84 ellipsoid), metres --
  not barometric. This is the input the WGS-84 ellipsoidal geodetic-to-ECEF transform
  (`crate::plugin::adsb::geodetic_to_ecef_m`) actually needs; see that module's own doc
  comment for why barometric altitude would be the wrong quantity here.

## How every row's values were generated

Three aircraft, each moving along a straight-line analytic path in geodetic coordinates
(no noise, no randomness -- every value below is exactly reproducible by hand from these
six numbers per aircraft): `lat(e) = lat0 + dlat*e`, `lon(e) = lon0 + dlon*e`, `alt(e) =
alt0 + dalt*e`, `timestamp(e) = t0 + dt*e`, for epoch index `e = 0..14` (15 epochs per
aircraft), `t0 = 1_700_000_000` (Unix seconds), `dt = 5` (seconds):

| `icao24`  | `lat0`   | `dlat`   | `lon0`     | `dlon`   | `alt0`    | `dalt` |
|-----------|----------|----------|------------|----------|-----------|--------|
| `a00001`  | 34.0000  | +0.0100  | -118.0000  | +0.0150  | 10000.0   | +5.0   |
| `a00002`  | 40.5000  | -0.0080  | -73.5000   | +0.0120  | 9500.0    | -3.0   |
| `a00003`  | 51.4700  | +0.0050  | -0.4500    | -0.0200  | 11000.0   | +2.0   |

Every increment is a clean decimal (2-4 decimal places), so `lat(e)`/`lon(e)`/`alt(e)` are
exact decimals with no floating-point rounding for any `e` in range -- hand-checkable
directly from the table above and the row's own line number (`e = (utc_unix_s -
1_700_000_000) / 5`).

To regenerate (pure Python, no dependencies, the exact loop the committed file was
produced from):

```python
t0, dt = 1_700_000_000, 5
aircraft = [
    ("a00001", 34.0000, 0.0100, -118.0000, 0.0150, 10000.0, 5.0),
    ("a00002", 40.5000, -0.0080, -73.5000, 0.0120, 9500.0, -3.0),
    ("a00003", 51.4700, 0.0050, -0.4500, -0.0200, 11000.0, 2.0),
]
rows = {}
for icao, lat0, dlat, lon0, dlon, alt0, dalt in aircraft:
    for e in range(15):
        rows[(icao, e)] = (icao, t0 + dt * e, round(lat0 + dlat * e, 4), round(lon0 + dlon * e, 4), round(alt0 + dalt * e, 1))
```

Rows are written interleaved by epoch (`a00001[e]`, `a00002[e]`, `a00003[e]`, for `e =
0..14`), which is itself what puts multiple aircraft on the same epoch -- all three
aircraft share every one of the 15 epochs, so every batch this fixture produces
(`BatchingRule::PerEpoch`) carries exactly three measurements. Two of `a00002`'s rows
(epoch 6 and epoch 7) are then swapped from their natural position, so the file is **not**
fully sorted by timestamp for that aircraft -- `AdsbCsvSource::from_csv` groups by epoch
into a `BTreeMap` regardless of input order (`crate::plugin::adsb`'s own doc comment), and
this row swap is what actually exercises that rather than merely asserting it.

## The one row that must be REFUSED

The file's last line,

```
a00001,1700000075,95.5000,-117.7750,10075.0
```

declares `lat_deg = 95.5`, outside the valid `[-90, 90]` range -- refused by
`AdsbCsvSource::from_csv` as `PluginError::AdsbLatitudeOutOfRange { line: 47, lat_deg: 95.5
}`, never silently dropped. It is placed as the file's own trailing line specifically so it
can be excluded by a plain "all lines but the last" slice for the tests that need a fully
valid replay (`crates/av-edge/tests/plugin_replay_adsb.rs::valid_fixture_csv_bytes`,
`crates/av-kernel/tests/edge_plugin_adsb_ecef_crosscheck.rs::valid_rows`) while the full,
unmodified committed file is what
`crates/av-edge/tests/plugin_replay_adsb.rs::the_fixtures_one_bad_row_is_a_typed_refusal_
not_a_silent_skip` feeds straight into the parser to prove the refusal fires on a real
fixture row, not only a synthetic in-test string.

## File shape

- 47 lines total: 1 header + 46 data lines (45 valid + 1 refused).
- 3 aircraft x 15 epochs = 45 valid rows, spanning `utc_unix_s` 1700000000..1700000070
  (70 s, 5 s cadence) plus the one refused row's own out-of-range-only epoch
  (1700000075, one past the last valid epoch, so it never collides with a real one).
- SHA-256 of `sample.csv`, exactly as committed: `c116bc4113f8914d4c54a7670164945967223a7e213cd9a8b551a42bf1fede68`
  (informational only -- no test in this crate re-verifies this hash before parsing; unlike
  `tests/fixtures/ground_segment/port_traffic.pb`, this fixture is read directly by every
  test, with no separate `RunProducts`-style manifest hash to check it against).

## Pinned values

`crates/av-edge/tests/plugin_replay_adsb.rs` runs the full CSV -> `AdsbCsvSource` ->
`BatchBuilder::build_batches_for_config` (`BatchingRule::PerEpoch`, `Pacing::
AsFastAsPossible`, `tests/fixtures/test_signing_key.pem`) path over the 45 valid rows and
pins:

- Measurement count: **45**.
- Batch count: **15** (one per distinct epoch, three aircraft per batch).
- Final chain head (`batch_hash` of the 15th batch), hex: `8f6bd8ce345cf4155b60f0126727c2e0ff2c4a8c8c198c3545d9eafa58158668`
- `PluginConfig::config_hash()` (of the exact `PluginConfig` that file's own `config()`
  builds), hex: `a550bf530c860876ed51f371221b0092770ab01572b07a8819ec01d2e26977d5`

`crates/av-kernel/tests/edge_plugin_adsb_ecef_crosscheck.rs` cross-checks `crate::plugin::
adsb::geodetic_to_ecef_m`'s output for these same 45 rows against `av_kernel::drm::ground::
geodetic_to_ecef_m` and prints the worst deviation: **`0e0` m** (both sides implement the
identical WGS-84 closed-form transform, so bit-for-bit agreement is expected here, not
merely "small").

If `crates/av-edge/src/plugin/adsb.rs`'s parsing/conversion logic, `tests/fixtures/
test_signing_key.pem`, or this fixture ever change, the four pinned values above must be
recomputed and repinned together -- `plugin_replay_adsb.rs`'s own tests print the actual
measurement count, batch count, chain head and config hash on every run, so repinning is
"read the printed value, paste it here."
