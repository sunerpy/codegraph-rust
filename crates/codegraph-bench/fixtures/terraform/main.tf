terraform {}

provider "aws" {}

variable "region" {}

locals {
  name   = "demo"
  tagged = var.region
}

data "aws_ami" "ubuntu" {}

resource "aws_s3_bucket" "b" {
  bucket     = var.region
  kms_key_id = aws_kms_key.logs.arn
}

module "vpc" {
  source = "./vpc"
  region = var.region
}

output "bucket_id" {
  value = aws_s3_bucket.b.id
}

output "vpc_id" {
  value = module.vpc.id
}
