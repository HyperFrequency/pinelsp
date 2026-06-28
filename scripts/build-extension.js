#!/usr/bin/env node

/**
 * Build script for the VS Code extension.
 * Bundles the extension client and LSP server using esbuild.
 */

const esbuild = require("esbuild");
const fs = require("node:fs");
const path = require("node:path");

const production = process.argv.includes("--production");
const watch = process.argv.includes("--watch");

// Ensure output directories exist
const outDir = path.join(__dirname, "..", "dist");
const vscodeDist = path.join(outDir, "packages", "vscode", "src");
const lspDist = path.join(outDir, "packages", "lsp", "bin");
const pineDataDist = path.join(outDir, "pine-data", "v6");

fs.mkdirSync(vscodeDist, { recursive: true });
fs.mkdirSync(lspDist, { recursive: true });
fs.mkdirSync(pineDataDist, { recursive: true });

// Copy JSON files from pine-data
const pineDataSrc = path.join(__dirname, "..", "pine-data", "v6");
for (const file of fs.readdirSync(pineDataSrc)) {
	if (file.endsWith(".json")) {
		fs.copyFileSync(
			path.join(pineDataSrc, file),
			path.join(pineDataDist, file),
		);
	}
}

// Common build options
const commonOptions = {
	bundle: true,
	platform: "node",
	target: "node18",
	format: "cjs",
	sourcemap: !production,
	minify: production,
	treeShaking: true,
};

// Build the extension client
const extensionConfig = {
	...commonOptions,
	entryPoints: ["packages/vscode/src/extension.ts"],
	outfile: "dist/packages/vscode/src/extension.js",
	external: ["vscode"], // vscode is provided by VS Code runtime
};

// Build the LSP server
const lspConfig = {
	...commonOptions,
	entryPoints: ["packages/lsp/bin/pine-lsp.ts"],
	outfile: "dist/packages/lsp/bin/pine-lsp.js",
	external: [], // Bundle everything for the LSP server
};

// Build the MCP server
const mcpConfig = {
	...commonOptions,
	entryPoints: ["packages/mcp/bin/pine-mcp.ts"],
	outfile: "dist/packages/mcp/bin/pine-mcp.js",
	external: [], // Bundle everything for the MCP server
};

// Build the CLI
const cliConfig = {
	...commonOptions,
	entryPoints: ["packages/cli/src/cli.ts"],
	outfile: "dist/packages/cli/src/cli.js",
	external: [],
	banner: {
		js: "#!/usr/bin/env node",
	},
};

// Copy a locally-built Rust `pine-lsp` binary into dist/bin/ if one exists.
// Wrapped so a missing binary (the common case in a TS-only build) only logs a
// note and never fails the build.
function copyBundledRustServer() {
	try {
		const isWindows = process.platform === "win32";
		const binName = `pine-lsp${isWindows ? ".exe" : ""}`;
		const releaseBin = path.join(
			__dirname,
			"..",
			"pine-rs",
			"target",
			"release",
			binName,
		);
		if (!fs.existsSync(releaseBin)) {
			console.log(
				`No Rust pine-lsp binary at ${releaseBin} (skipping bundle; TS server will be used)`,
			);
			return;
		}
		const binDir = path.join(outDir, "bin");
		fs.mkdirSync(binDir, { recursive: true });
		const dest = path.join(binDir, binName);
		fs.copyFileSync(releaseBin, dest);
		fs.chmodSync(dest, 0o755);
		console.log(`Bundled Rust pine-lsp binary into ${dest}`);
	} catch (error) {
		console.log(`Skipping Rust binary bundle (non-fatal): ${error.message}`);
	}
}

async function build() {
	try {
		if (watch) {
			// Watch mode
			const contexts = await Promise.all([
				esbuild.context(extensionConfig),
				esbuild.context(lspConfig),
				esbuild.context(mcpConfig),
				esbuild.context(cliConfig),
			]);

			await Promise.all(contexts.map((ctx) => ctx.watch()));
			console.log("Watching for changes...");
		} else {
			// One-time build
			await Promise.all([
				esbuild.build(extensionConfig),
				esbuild.build(lspConfig),
				esbuild.build(mcpConfig),
				esbuild.build(cliConfig),
			]);

			// Make CLI executable
			fs.chmodSync("dist/packages/cli/src/cli.js", 0o755);

			// Best-effort: bundle a locally-built Rust `pine-lsp` binary into
			// dist/bin/ so the extension can prefer it (see extension.ts). This is
			// a DEV convenience only — the marketplace per-platform packaging story
			// lives in .github/workflows/rust-release.yml, which builds the correct
			// binary for each target. Copying the host-arch debug/release binary
			// here MUST NOT be used to ship a VSIX (it would be wrong-arch on other
			// platforms). Absent binary => log a note and continue (never throws).
			copyBundledRustServer();

			console.log("Build complete!");
		}
	} catch (error) {
		console.error("Build failed:", error);
		process.exit(1);
	}
}

build();
