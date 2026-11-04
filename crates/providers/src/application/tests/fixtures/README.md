Answers the fake servers give, in the services' own shape. Written on
2026-10-24 from the published schemas, not recorded live:

- `ipinfo.json`: ipinfo.io's `/json` answer, with a documentation IP
  (`192.0.2.1`) and Copenhagen's coordinates;
- `latest.json`, `forecast.json`: Electricity Maps v3 `CarbonIntensity` and
  forecast answers, from `https://api.electricitymaps.com/v3/openapi.json`.
  The forecast's datetimes are moved to the present by the tests.

When a live test (`tests/live_*.rs`) fails on shape, re-record these from the
live answer, replacing any real IP and location with the ones above.
