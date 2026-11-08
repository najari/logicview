@echo off
rem Start the logicview MCP server from the portable SLogicView folder.
rem   start-mcp.cmd            HTTP on http://127.0.0.1:10530 (like Logic 2)
rem   start-mcp.cmd --stdio    stdio transport (for clients that launch servers)
rem   start-mcp.cmd --port N   another port
"%~dp0..\python.exe" -I "%~dp0logicview_mcp.py" %*
