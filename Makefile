.PHONY: help install install-dev install-test sync lint format check fix test test-unit test-integration test-cov run-host run-client run-gui clean

help:  ## Show this help message
	@echo 'Usage: make [target]'
	@echo ''
	@echo 'Available targets:'
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-20s %s\n", $$1, $$2}'

install:  ## Install project dependencies using uv
	uv pip install -e .

install-dev:  ## Install project with dev dependencies
	uv pip install -e ".[dev]"

install-test:  ## Install project with test dependencies
	uv pip install -e ".[test]"

sync:  ## Sync dependencies with pyproject.toml
	uv pip sync

lint:  ## Run ruff linter
	uv run ruff check src tests

format:  ## Format code with ruff
	uv run ruff format src tests

check:  ## Run linter without making changes
	uv run ruff check src tests --no-fix

fix:  ## Auto-fix linting issues
	uv run ruff check src tests --fix
	uv run ruff format src tests

test:  ## Run all tests
	uv run pytest tests/ -v

test-unit:  ## Run unit tests only
	uv run pytest tests/ -v -m "not integration"

test-integration:  ## Run integration tests only
	uv run pytest tests/ -v -m integration

test-cov:  ## Run tests with coverage report
	uv run pytest tests/ --cov=src --cov-report=html --cov-report=term-missing

run-host:  ## Run the host application
	uv run linuxplay-host --gui

run-client:  ## Run the client application
	uv run linuxplay-client --help

run-gui:  ## Run the GUI launcher
	uv run linuxplay

clean:  ## Clean up cache and build artifacts
	find . -type d -name "__pycache__" -exec rm -rf {} + 2>/dev/null || true
	find . -type d -name "*.egg-info" -exec rm -rf {} + 2>/dev/null || true
	find . -type f -name "*.pyc" -delete
	find . -type f -name "*.pyo" -delete
	rm -rf .ruff_cache build dist .pytest_cache htmlcov .coverage
