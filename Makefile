.PHONY: help install install-dev sync lint format check fix run-host run-client run-gui clean

help:  ## Show this help message
	@echo 'Usage: make [target]'
	@echo ''
	@echo 'Available targets:'
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

install:  ## Install project dependencies using uv
	uv pip install -e .

install-dev:  ## Install project with dev dependencies
	uv pip install -e ".[dev]"

sync:  ## Sync dependencies with pyproject.toml
	uv pip sync

lint:  ## Run ruff linter
	uv run ruff check .

format:  ## Format code with ruff
	uv run ruff format .

check:  ## Run linter without making changes
	uv run ruff check . --no-fix

fix:  ## Auto-fix linting issues
	uv run ruff check . --fix
	uv run ruff format .

run-host:  ## Run the host application
	uv run python host.py --gui

run-client:  ## Run the client application
	uv run python client.py --help

run-gui:  ## Run the GUI launcher
	uv run python start.py

clean:  ## Clean up cache and build artifacts
	find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name "*.egg-info" -exec rm -rf {} + 2>/dev/null || true
	find . -type f -name "*.pyc" -delete
	find . -type f -name "*.pyo" -delete
	rm -rf .ruff_cache build dist
