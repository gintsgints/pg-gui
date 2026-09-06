-- Stored-procedure debugger API; the plugin_debugger library is preloaded
-- via the compose command and built into the image (see docker/Dockerfile).
CREATE EXTENSION IF NOT EXISTS pldbgapi;
