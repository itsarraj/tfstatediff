# tfstatediff

Diffs two Terraform state (`.tfstate`) snapshots: which resources were
created, destroyed, or drifted (attribute-level, not just "changed"),
and which outputs changed — with sensitive values redacted, never
printed.

This is a different job from this workspace's `tfplanshow`, which
renders a *plan* (`terraform show -json` on a plan file — what
Terraform is *about* to do). `tfstatediff` compares two *state*
snapshots — what Terraform already believes is true, at two points in
time (e.g. a state file from last week vs. today, or before/after an
out-of-band change).

## Usage

```bash
tfstatediff old.tfstate new.tfstate
```

Exit code `0` means no differences; `1` means differences were found;
`2` means one of the files couldn't be read or parsed.

## How it works

Parses Terraform's state v4 JSON format (the format used since
Terraform 0.12) directly — no dependency on the `terraform` binary
being installed, since state files are just JSON on disk. Builds each
resource instance's real Terraform address (`module.vpc.aws_subnet.private["us-east-1a"]`,
`data.aws_ami.ubuntu`, `aws_instance.worker[2]` — module prefix, `data.`
prefix for data sources, `count`/`for_each` index suffix, matching what
`terraform state list` would show), matches instances across the two
files by that address, and recursively diffs their attribute JSON
leaf-by-leaf (dotted-path notation: `tags.Name`, `ingress[0].from_port`).
Outputs are compared the same way.

**Sensitive values are never printed.** Any leaf whose path's final
segment matches a common sensitive keyword (`password`, `secret`,
`token`, `private_key`, `access_key`, `credential`, `api_key`) is
redacted on both sides, and any output marked `"sensitive": true` in
the state is redacted regardless of its name. Redaction is applied at
render time — the underlying values are still compared for equality,
so a sensitive value that *changed* is correctly reported as changed,
just without ever showing what it changed to or from.

## Status: built, unit-tested, and verified against a realistic hand-built state fixture covering every change type at once

Terraform itself isn't installed in this environment (would require a
system package install this pass deliberately avoided), so — matching
this workspace's `tfplanshow`, which hit the same constraint —
verification here means a thorough, schema-accurate hand-built fixture
rather than output from a live `terraform apply`, checked against
hand-computed expected results rather than just eyeballed.

- **19 unit tests** (`cargo test --lib`) cover state parsing (mode
  defaulting to `"managed"` when absent, since older state files
  sometimes omit it), every resource address shape (root, data source,
  module-prefixed, `for_each` string index, `count` numeric index,
  module+data combined), attribute flattening (nested objects, arrays),
  attribute diffing (changed/added/removed leaves, unchanged leaves
  correctly omitted), sensitive-path redaction, resource-level
  created/destroyed/changed classification, and output diffing.
  **A real bug caught by the tests, not a test-expectation error**: the
  first `diff_outputs` implementation compared the *already-redacted*
  display strings between old and new — since every sensitive value
  redacts to the identical fixed marker, a genuinely *changed*
  sensitive output (e.g. a rotated database password) would silently
  compare equal and vanish from the diff entirely, hiding a real
  change. Fixed by comparing the raw value (plus the `sensitive` flag)
  first, and only redacting for the final display string — the
  comparison and the redaction were wrongly using the same pass.
- **Verified against a realistic 4-resource, 2-output state pair**
  covering every change type in one run: an EC2 instance resized
  (`t3.micro` → `t3.large`) with one tag changed and one left alone: the
  tool correctly reported *only* `instance_type` and `tags.Env`, not
  `tags.Name`; an RDS instance with its `instance_class` changed *and*
  its `sensitive_attributes`-marked `password` rotated: correctly shows
  `instance_class` in the clear and `password` as `(sensitive value,
  redacted) -> (sensitive value, redacted)` on both sides — proving the
  bug-fix above works end-to-end, not just in isolation; a `count`-indexed
  resource shrinking from 2 instances to 1: correctly reported only
  `aws_instance.worker[1]` as destroyed, `worker[0]` correctly omitted
  as unchanged; a resource removed entirely (`aws_s3_bucket.logs`) and
  one added (`aws_security_group.web_sg`): correctly `destroyed` and
  `created`; a sensitive output (`db_password`) that rotated: correctly
  shown as changed with both sides redacted; a plain output
  (`instance_ip`) that changed: shown in the clear. Every line of
  output matched a hand-computed expectation exactly. Also verified:
  diffing a state file against itself produces "No resource changes" /
  "No output changes" and exit code 0; a `lineage` mismatch between the
  two files prints an explicit warning that the snapshots may not share
  a history; a non-JSON file and a nonexistent path both fail cleanly
  with exit code 2 instead of panicking.

**Not done / deliberately deferred**: no live verification against
Terraform's actual CLI output (see above); `sensitive_attributes`'
per-instance list is not parsed for exact-path redaction — only the
keyword heuristic and the top-level output `sensitive` flag are used,
since the list's internal structure has changed across state format
minor revisions and a heuristic miss would be a silent under-redaction
risk, so this deliberately over-redacts by keyword rather than
under-redacts by relying on an exact but version-fragile match; no
`check_results` (Terraform's newer `terraform test`/check-block state)
diffing.
