.PHONY: help install install-dev install-test sync lint format check fix test test-unit test-integration test-cov run-host run-client run-gui clean

.DEFAULT_GOAL := help

help:
	@echo 'Usage: make [target]'
	@echo ''
	@echo 'Available targets:'
	@grep -E '^[a-zA-Z_-]+:' $(MAKEFILE_LIST) | awk 'BEGIN {FS=":"}; {printf "  %-20s\n", $$1}'

install:
	uv pip install -e .

install-dev:
	uv pip install -e ".[dev]"

install-test:
	uv pip install -e ".[test]"

sync:
	uv pip sync

lint:
	uv run ruff check src tests

format:
	uv run ruff format src tests

check:
	uv run ruff check src tests --no-fix

fix:
	uv run ruff check src tests --fix
	uv run ruff format src tests

test:
	uv run pytest tests -v

test-unit:
	uv run pytest tests -v -m "not integration"

test-integration:
	uv run pytest tests -v -m integration

test-cov:
	uv run pytest tests --cov=src --cov-report=html --cov-report=term-missing

run-host:
	uv run linuxplay-host --gui

run-client:
	uv run linuxplay-client --help

run-gui:
	uv run linuxplay

clean:
	find . -type d -name "__pycache__" -exec rm -rf {} + || true
	find . -type d -name "*.egg-info" -exec rm -rf {} + || true
	find . -type f -name "*.py[co]" -delete || true
	rm -rf .ruff_cache build dist .pytest_cache htmlcov .coverage || true
