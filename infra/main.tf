# Taisce server: one t4g.micro behind Portus (TLS via its own ACME, tls-alpn-01),
# 443 the only open port, managed through SSM (no SSH), backups to S3.
# Personal AWS account only — the provider refuses any other account.

terraform {
  required_version = ">= 1.13.0"
  # State lives off the laptop in its own versioned bucket (created by hand:
  # the box's role can reach the backups bucket, never this one).
  backend "s3" {
    bucket       = "taisce-tfstate-895102116452"
    key          = "taisce/terraform.tfstate"
    region       = "eu-west-1"
    encrypt      = true
    use_lockfile = true
  }
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.67"
    }
  }
}

provider "aws" {
  region              = var.region
  allowed_account_ids = [var.account_id]
  default_tags {
    tags = { Project = "taisce", ManagedBy = "opentofu" }
  }
}

variable "account_id" {
  type    = string
  default = "895102116452"
}

variable "region" {
  type    = string
  default = "eu-west-1"
}

variable "zone_name" {
  type    = string
  default = "null.ie"
}

variable "hostname" {
  type    = string
  default = "taisce.null.ie"
}

variable "instance_type" {
  type    = string
  default = "t4g.micro"
}

# ---- network ---------------------------------------------------------------

data "aws_vpc" "default" {
  default = true
}

data "aws_subnets" "default_az_a" {
  filter {
    name   = "vpc-id"
    values = [data.aws_vpc.default.id]
  }
  filter {
    name   = "availability-zone"
    values = ["${var.region}a"]
  }
  filter {
    name   = "default-for-az"
    values = ["true"]
  }
}

resource "aws_security_group" "taisce" {
  name        = "taisce"
  description = "HTTPS only; management via SSM"
  vpc_id      = data.aws_vpc.default.id
}

resource "aws_vpc_security_group_ingress_rule" "https_v4" {
  security_group_id = aws_security_group.taisce.id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
  description       = "HTTPS (Portus; tls-alpn-01 validation rides this too)"
}

resource "aws_vpc_security_group_egress_rule" "all_v4" {
  security_group_id = aws_security_group.taisce.id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "-1"
  description       = "Outbound - Lets Encrypt, APNs, S3, SSM, package repos"
}

# ---- backups ---------------------------------------------------------------

resource "aws_s3_bucket" "backups" {
  bucket = "taisce-backups-${var.account_id}"
}

resource "aws_s3_bucket_public_access_block" "backups" {
  bucket                  = aws_s3_bucket.backups.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_versioning" "backups" {
  bucket = aws_s3_bucket.backups.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "backups" {
  bucket = aws_s3_bucket.backups.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "backups" {
  bucket = aws_s3_bucket.backups.id
  rule {
    id     = "expire-old-versions"
    status = "Enabled"
    filter {}
    noncurrent_version_expiration {
      noncurrent_days = 30
    }
    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }
}

# ---- instance role: SSM + the backup bucket, nothing else -------------------

data "aws_iam_policy_document" "ec2_assume" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "taisce" {
  name               = "taisce-server"
  assume_role_policy = data.aws_iam_policy_document.ec2_assume.json
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.taisce.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

data "aws_iam_policy_document" "backups" {
  statement {
    actions   = ["s3:ListBucket", "s3:GetBucketLocation"]
    resources = [aws_s3_bucket.backups.arn]
  }
  statement {
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.backups.arn}/*"]
  }
}

resource "aws_iam_role_policy" "backups" {
  name   = "taisce-backups"
  role   = aws_iam_role.taisce.id
  policy = data.aws_iam_policy_document.backups.json
}

data "aws_iam_policy_document" "apns" {
  statement {
    actions   = ["ssm:GetParameter", "ssm:GetParameters"]
    resources = ["arn:aws:ssm:${var.region}:${var.account_id}:parameter/taisce/apns/*"]
  }
}

resource "aws_iam_role_policy" "apns" {
  name   = "taisce-apns-key"
  role   = aws_iam_role.taisce.id
  policy = data.aws_iam_policy_document.apns.json
}

resource "aws_iam_instance_profile" "taisce" {
  name = "taisce-server"
  role = aws_iam_role.taisce.name
}

# ---- the box ---------------------------------------------------------------

data "aws_ssm_parameter" "al2023_arm64" {
  name = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-arm64"
}

resource "aws_instance" "taisce" {
  ami                    = data.aws_ssm_parameter.al2023_arm64.value
  instance_type          = var.instance_type
  subnet_id              = data.aws_subnets.default_az_a.ids[0]
  vpc_security_group_ids = [aws_security_group.taisce.id]
  iam_instance_profile   = aws_iam_instance_profile.taisce.name
  user_data              = file("${path.module}/user-data.sh")

  metadata_options {
    http_tokens   = "required"
    http_endpoint = "enabled"
  }

  root_block_device {
    volume_type           = "gp3"
    volume_size           = 10
    encrypted             = true
    delete_on_termination = false
    # default_tags reach the volume at create but not on import; say them here
    tags = { Project = "taisce", ManagedBy = "opentofu" }
  }

  # A new AMI release must not replace the box (and its data) on the next apply.
  lifecycle {
    ignore_changes = [ami, user_data]
  }

  tags = { Name = "taisce" }
}

resource "aws_eip" "taisce" {
  domain = "vpc"
  tags   = { Name = "taisce" }
}

resource "aws_eip_association" "taisce" {
  instance_id   = aws_instance.taisce.id
  allocation_id = aws_eip.taisce.id
}

# ---- DNS -------------------------------------------------------------------

data "aws_route53_zone" "root" {
  name = var.zone_name
}

resource "aws_route53_record" "taisce" {
  zone_id = data.aws_route53_zone.root.zone_id
  name    = var.hostname
  type    = "A"
  ttl     = 300
  records = [aws_eip.taisce.public_ip]
}

output "instance_id" {
  value = aws_instance.taisce.id
}

output "public_ip" {
  value = aws_eip.taisce.public_ip
}

output "url" {
  value = "https://${var.hostname}"
}

output "backup_bucket" {
  value = aws_s3_bucket.backups.bucket
}
