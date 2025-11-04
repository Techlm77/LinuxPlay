# LinuxPlay Test Suite

This directory contains the test suite for LinuxPlay. The tests are organized into unit tests and integration tests to ensure code quality and reliability.

## Test Structure

```
tests/
├── __init__.py              # Test package initialization
├── conftest.py              # Pytest configuration and shared fixtures
├── test_host_utils.py       # Unit tests for host.py utilities
├── test_client_utils.py     # Unit tests for client.py utilities
├── test_auth.py             # Tests for authentication and security
└── test_integration.py      # Integration tests for network communication
```

## Test Categories

### Unit Tests
- **test_host_utils.py**: Tests for host utility functions
  - Bitrate parsing and formatting
  - PIN generation and validation
  - NVENC preset and tune mapping
  - QP normalization
  - MPEG-TS packet size calculation
  - VAAPI format selection
  - Target BPP calculation
  
- **test_client_utils.py**: Tests for client utility functions
  - Network mode detection (LAN/WiFi)
  - Hardware acceleration selection
  - Client state management
  - Renderer selection
  - Key mapping
  
- **test_auth.py**: Tests for authentication and security
  - PIN management and rotation
  - Certificate authority setup
  - Trusted clients database
  - Certificate fingerprint handling
  - Host state management

### Integration Tests
- **test_integration.py**: Tests for network communication
  - UDP socket communication
  - TCP handshake mechanism
  - Heartbeat protocol (PING/PONG)
  - Control message protocol
  - Clipboard synchronization
  - Stream thread management
  - Monitor detection

## Running Tests

### Install Test Dependencies

```bash
# Using uv (recommended)
make install-test

# Or directly
uv pip install -e ".[test]"
```

### Run All Tests

```bash
make test
# Or
uv run pytest tests/ -v
```

### Run Unit Tests Only

```bash
make test-unit
# Or
uv run pytest tests/ -v -m "not integration"
```

### Run Integration Tests Only

```bash
make test-integration
# Or
uv run pytest tests/ -v -m integration
```

### Run Tests with Coverage

```bash
make test-cov
# Or
uv run pytest tests/ --cov=. --cov-report=html --cov-report=term-missing
```

This will generate:
- Terminal coverage report
- HTML coverage report in `htmlcov/` directory

### Run Specific Test File

```bash
uv run pytest tests/test_host_utils.py -v
```

### Run Specific Test Class

```bash
uv run pytest tests/test_host_utils.py::TestBitrateUtils -v
```

### Run Specific Test Function

```bash
uv run pytest tests/test_host_utils.py::TestBitrateUtils::test_parse_bitrate_with_k_suffix -v
```

## Test Markers

Tests can be marked with pytest markers:

- `@pytest.mark.unit` - Unit tests (default for non-integration tests)
- `@pytest.mark.integration` - Integration tests that test component interactions
- `@pytest.mark.slow` - Tests that take longer to run

### Running Tests by Marker

```bash
# Run only integration tests
uv run pytest -m integration

# Skip slow tests
uv run pytest -m "not slow"

# Run unit tests only
uv run pytest -m "not integration"
```

## Writing New Tests

### Test File Naming
- Test files must start with `test_`
- Example: `test_new_feature.py`

### Test Class Naming
- Test classes must start with `Test`
- Example: `class TestNewFeature:`

### Test Function Naming
- Test functions must start with `test_`
- Example: `def test_feature_works():`

### Example Test

```python
"""Tests for new feature."""

import pytest


class TestNewFeature:
    """Tests for the new feature."""

    def test_basic_functionality(self):
        """Test basic functionality."""
        from module import new_feature
        
        result = new_feature("input")
        assert result == "expected_output"

    def test_error_handling(self):
        """Test error handling."""
        from module import new_feature
        
        with pytest.raises(ValueError):
            new_feature(None)

    @pytest.mark.integration
    def test_integration(self):
        """Test integration with other components."""
        # Integration test code
        pass
```

## Fixtures

Shared test fixtures are defined in `conftest.py`:

- `mock_ffmpeg_available` - Mocks FFmpeg as available
- `mock_linux_platform` - Mocks platform as Linux
- `mock_windows_platform` - Mocks platform as Windows

### Using Fixtures

```python
def test_with_fixture(mock_linux_platform):
    """Test using a fixture."""
    import platform
    assert platform.system() == "Linux"
```

## Continuous Integration

Tests can be run in CI/CD pipelines:

```yaml
# Example GitHub Actions workflow
- name: Install test dependencies
  run: make install-test

- name: Run tests with coverage
  run: make test-cov

- name: Upload coverage
  uses: codecov/codecov-action@v3
```

## Coverage Goals

- **Overall coverage**: Target 70%+ for critical code paths
- **Unit tests**: Cover all utility functions and core logic
- **Integration tests**: Cover major communication protocols

## Test Best Practices

1. **Isolate tests**: Each test should be independent
2. **Use fixtures**: Share common setup code via fixtures
3. **Clear assertions**: Use descriptive assertion messages
4. **Mock external dependencies**: Mock file I/O, network calls, etc.
5. **Test edge cases**: Include boundary conditions and error cases
6. **Fast tests**: Keep unit tests fast (< 1 second each)
7. **Descriptive names**: Use clear, descriptive test names

## Debugging Tests

### Run with more verbose output

```bash
uv run pytest tests/ -vv
```

### Show print statements

```bash
uv run pytest tests/ -s
```

### Stop at first failure

```bash
uv run pytest tests/ -x
```

### Drop into debugger on failure

```bash
uv run pytest tests/ --pdb
```

### Run last failed tests

```bash
uv run pytest tests/ --lf
```

## Contributing

When contributing new features:

1. Write tests for new functionality
2. Ensure all existing tests pass
3. Aim for high test coverage
4. Follow existing test patterns
5. Mark integration tests appropriately
6. Update this README if adding new test categories

## Dependencies

Testing dependencies (installed with `[test]` extra):

- **pytest**: Test framework
- **pytest-cov**: Coverage reporting
- **pytest-mock**: Mocking support
- **pytest-timeout**: Timeout handling for long-running tests

## Troubleshooting

### Import errors
- Ensure you've installed the package: `uv pip install -e ".[test]"`
- Check that you're running tests from the project root

### Module not found
- The project root is added to `pythonpath` in `pyproject.toml`
- Ensure `conftest.py` is present

### Tests hanging
- Use `pytest-timeout` to set timeouts
- Check for infinite loops or blocking operations

### Platform-specific failures
- Some tests may require specific platforms (Linux/Windows)
- Use platform mocking fixtures for cross-platform testing
