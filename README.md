# ecr-dump

A small tool to dump all image manifests from ECR. Uses default AWS creds

```shell
$ export AWS_PROFILE=... AWS_REGION=...
$ cargo run --release -- manifests.jsonl
```
