#!/bin/bash
# Quick Test Runner for LinuxPlay
# This script helps you quickly run different test suites

set -e

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Colours

echo -e "${BLUE}╔════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║      LinuxPlay Test Suite Runner       ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════╝${NC}"
echo

# Ensure ~/.local/bin is on PATH (uv default install location)
if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    export PATH="$HOME/.local/bin:$PATH"
fi

# Check if uv is installed
if ! command -v uv &> /dev/null; then
    echo -e "${RED}✗ uv is not installed${NC}"
    echo "Install it with: curl -LsSf https://astral.sh/uv/install.sh | sh"
    echo "Or ensure ~/.local/bin is in your PATH"
    exit 1
fi

echo -e "${GREEN}✓ uv is installed${NC}"

# Check if test dependencies are installed
if ! uv pip show pytest &> /dev/null; then
    echo -e "${YELLOW}⚠ Test dependencies not installed${NC}"
    echo "Installing test dependencies..."
    uv pip install -e ".[test]"
else
    echo -e "${GREEN}✓ Test dependencies installed${NC}"
fi

echo

# Show menu
echo "Select test suite to run:"
echo "  1) All tests"
echo "  2) Unit tests only"
echo "  3) Integration tests only"
echo "  4) Tests with coverage report"
echo "  5) Fast check (skip integration)"
echo "  6) Specific test file"
echo "  q) Quit"
echo

read -p "Enter choice [1-6 or q]: " choice

case $choice in
    1)
        echo -e "\n${BLUE}Running all tests...${NC}\n"
        uv run pytest tests/ -v
        ;;
    2)
        echo -e "\n${BLUE}Running unit tests only...${NC}\n"
        uv run pytest tests/ -v -m "not integration"
        ;;
    3)
        echo -e "\n${BLUE}Running integration tests only...${NC}\n"
        uv run pytest tests/ -v -m integration
        ;;
    4)
        echo -e "\n${BLUE}Running tests with coverage...${NC}\n"
        uv run pytest tests/ --cov=. --cov-report=html --cov-report=term-missing
        echo -e "\n${GREEN}Coverage report generated in htmlcov/index.html${NC}"
        ;;
    5)
        echo -e "\n${BLUE}Running fast unit tests...${NC}\n"
        uv run pytest tests/ -v -m "not integration and not slow"
        ;;
    6)
        echo
        echo "Available test files:"
        echo "  1) test_host_utils.py"
        echo "  2) test_client_utils.py"
        echo "  3) test_auth.py"
        echo "  4) test_integration.py"
        read -p "Enter file number: " file_choice
        
        case $file_choice in
            1) file="test_host_utils.py" ;;
            2) file="test_client_utils.py" ;;
            3) file="test_auth.py" ;;
            4) file="test_integration.py" ;;
            *) echo -e "${RED}Invalid choice${NC}"; exit 1 ;;
        esac
        
        echo -e "\n${BLUE}Running tests in $file...${NC}\n"
        uv run pytest tests/$file -v
        ;;
    q|Q)
        echo "Exiting..."
        exit 0
        ;;
    *)
        echo -e "${RED}Invalid choice${NC}"
        exit 1
        ;;
esac

echo
echo -e "${GREEN}═══════════════════════════════════════${NC}"
echo -e "${GREEN}     Test run completed!${NC}"
echo -e "${GREEN}═══════════════════════════════════════${NC}"
