Hi Sam,

Thanks for the quick review. I went through your notes and **fixed the retry bug** before tagging the release, so the build from _this morning_ is the one to test.

What changed since Friday:

- Retries back off exponentially and stop after five attempts
- The `--dry-run` flag prints the plan without touching the database
- Timeouts are configurable per job

Next steps:

1. Run the migration on staging
2. Check the dashboards for an hour
3. Roll out to production on Thursday

The full changelog is on [the release page](https://example.com/releases/2.4). Let me know if anything looks off.

Best,
Alex
