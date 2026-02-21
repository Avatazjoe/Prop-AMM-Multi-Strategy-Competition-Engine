PYTHON ?= python3
PIP ?= pip3

.PHONY: setup setup-backend setup-dashboard test api worker dashboard dev dev-up dev-down dev-status docker-up docker-down

setup: setup-backend setup-dashboard

setup-backend:
	$(PYTHON) -m venv .venv
	. .venv/bin/activate && pip install -U pip && pip install -r requirements.txt

setup-dashboard:
	cd apps/dashboard && npm install

test:
	export PATH="$$HOME/.cargo/bin:$$PATH" && cargo test
	. .venv/bin/activate && PYTHONPATH=$$PWD $(PYTHON) -m compileall apps/backend/app apps/worker
	cd apps/dashboard && npm run build

api:
	. .venv/bin/activate && PYTHONPATH=$$PWD uvicorn apps.backend.app.main:app --host 0.0.0.0 --port $${PROP_AMM_API_PORT:-18002}

worker:
	. .venv/bin/activate && PYTHONPATH=$$PWD $(PYTHON) -m apps.worker.worker

dashboard:
	cd apps/dashboard && npm run dev

dev:
	@echo "Run in separate terminals: make api | make worker | make dashboard"

dev-up:
	bash scripts/dev-up.sh

dev-down:
	bash scripts/dev-down.sh

dev-status:
	bash scripts/dev-status.sh

docker-up:
	docker compose -f deploy/docker/docker-compose.yml up --build

docker-down:
	docker compose -f deploy/docker/docker-compose.yml down
