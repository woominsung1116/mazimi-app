.PHONY: dev dev-up dev-down dev-logs prod prod-up prod-down prod-logs db-shell scrape clean differential-full \
        deploy backup restore logs restart status

# Development
dev: dev-up
dev-up:
	docker compose -f compose.dev.yml up --build
dev-down:
	docker compose -f compose.dev.yml down
dev-logs:
	docker compose -f compose.dev.yml logs -f

# Production
prod: prod-up
prod-up:
	docker compose --env-file .env.production -f compose.yml up --build -d
prod-down:
	docker compose --env-file .env.production -f compose.yml down
prod-logs:
	docker compose --env-file .env.production -f compose.yml logs -f

# Scraper
scrape:
	docker compose -f compose.dev.yml run --rm scraper

# Utilities
db-shell:
	docker compose -f compose.dev.yml exec db psql -U wello -d wello
# Corporate reader differential gate, full mode (nightly or before a
# release): needs TEST_DATABASE_URL pointing at a throwaway Postgres.
differential-full:
	DIFF_FULL=1 cargo test -p mazimi-worker --lib differential_gate -- --nocapture --test-threads=1

# Cleanup
clean:
	docker compose -f compose.dev.yml down -v
	docker compose --env-file .env.production -f compose.yml down -v

# ── Deployment ────────────────────────────────────────────────────────────────

## deploy — full production deployment (build, migrate, start, health-check)
deploy:
	@bash scripts/deploy.sh

## backup — dump PostgreSQL to backups/wello_<timestamp>.sql.gz
backup:
	@bash scripts/backup.sh

## restore — verified interactive restore; usage: make restore BACKUP=backups/file.sql.gz
restore:
	@test -n "$(BACKUP)" || (echo "Usage: make restore BACKUP=backups/file.sql.gz" && exit 1)
	@bash scripts/restore.sh "$(BACKUP)"

## logs — tail all production service logs (Ctrl+C to stop)
logs:
	docker compose --env-file .env.production -f compose.yml logs -f

## restart — rolling restart of all production services
restart:
	docker compose --env-file .env.production -f compose.yml restart

## status — show container health and uptime
status:
	docker compose --env-file .env.production -f compose.yml ps
