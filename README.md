# ecr-dump

A small tool to dump all image manifests from ECR.

```shell
$ export AWS_PROFILE=... AWS_REGION=...
$ ./ecr-dump images.jsonl --exclude='foo/*'
```

This will output the manifests and layers for all images in all ECR repositories:

```json
{
  "image": {
    "repository_name": "foo/bar",
    "manifest_digest": "sha256:4ead720f67f34e4a430f4099d3f108cbf2324aaa53253fd1f0763add8f8158b0",
    "manifest_type": "Image",
    "image_tags": [
      "latest"
    ],
    "image_pushed_at": "2023-10-02T09:54:16Z"
  },
  "manifests": [
    {
      "content": {
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {
          "mediaType": "application/vnd.docker.container.image.v1+json",
          "digest": "sha256:7473ca8183c19cbe756821945a727a2ed38470046ececc6d9a942602430d9789",
          "size": 12449
        },
        "layers": [
          {
            "mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip",
            "digest": "sha256:7dbc1adf280e1aa588c033eaa746aa6db327ee16be705740f81741f5e6945c86",
            "size": 31417711
          }
        ]
      }
    }
  ]
}
```