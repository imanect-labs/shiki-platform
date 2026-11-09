terraform {
  required_version = ">= 1.14, < 2.0"
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 7.36"
    }
  }
}

# Intentionally local-only: no credentials, project IDs or endpoints from ADC.
provider "google" {
  project                          = "shiki-local"
  region                           = "us-central1"
  access_token                     = "floci-local-only"
  storage_custom_endpoint          = "http://127.0.0.1:4588/storage/v1/"
  pubsub_custom_endpoint           = "http://127.0.0.1:4588/v1/"
  container_custom_endpoint        = "http://container.localhost:4588/"
  resource_manager_custom_endpoint = "http://127.0.0.1:4588/v1/"
}

resource "google_container_cluster" "shiki" {
  name                = "shiki-local"
  location            = "us-central1"
  initial_node_count  = 1
  deletion_protection = false
}

# One shared bucket, not one bucket per tenant. Application tenant isolation
# still belongs to StorageService/AuthContext; this experiment does not test it.
resource "google_storage_bucket" "blobs" {
  name                        = "shiki-local-blobs"
  location                    = "US"
  uniform_bucket_level_access = true
  force_destroy               = true
}

# Optional experiment: Shiki's actual job queue is Postgres-backed jobq.
variable "enable_pubsub_experiment" {
  type    = bool
  default = false
}

resource "google_pubsub_topic" "objects" {
  count = var.enable_pubsub_experiment ? 1 : 0
  name  = "shiki-local-object-events"
}

resource "google_pubsub_subscription" "objects" {
  count                = var.enable_pubsub_experiment ? 1 : 0
  name                 = "shiki-local-object-events-smoke"
  topic                = google_pubsub_topic.objects[0].id
  ack_deadline_seconds = 30
}

resource "google_storage_notification" "objects" {
  count          = var.enable_pubsub_experiment ? 1 : 0
  bucket         = google_storage_bucket.blobs.name
  topic          = google_pubsub_topic.objects[0].id
  payload_format = "JSON_API_V1"
  event_types    = ["OBJECT_FINALIZE"]
  depends_on     = [google_pubsub_subscription.objects]
}

output "cluster_endpoint" {
  value = google_container_cluster.shiki.endpoint
}

output "bucket" {
  value = google_storage_bucket.blobs.name
}
