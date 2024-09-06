# ecr-dump

A small tool to dump all image manifests from ECR.

```shell
$ export AWS_PROFILE=... AWS_REGION=...
$ cargo run --release -- manifests.jsonl
```
