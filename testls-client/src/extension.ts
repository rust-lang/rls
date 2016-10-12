'use strict';

import * as path from 'path';

import { workspace, Disposable, ExtensionContext, languages } from 'vscode';
import { LanguageClient, LanguageClientOptions, SettingMonitor, ServerOptions, TransportKind } from 'vscode-languageclient';

export function activate(context: ExtensionContext) {
	let dir = __dirname.split(/(\\|\/)/g);
 	dir.pop();
 	dir.pop();
 	dir.pop();
 	let result_dir = dir.join("");
 
 	let serverOptions: ServerOptions = {
		run: {command: "cargo", args: ["run"], options: {cwd: result_dir}},
		debug: {command: "cargo", args: ["run"], options: {cwd: result_dir}}
	};
	// Options to control the language client
	let clientOptions: LanguageClientOptions = {
		// Register the server for Rust files
		documentSelector: ['rust'],
		synchronize: {
			// Synchronize the setting section 'languageServerExample' to the server
			configurationSection: 'languageServerExample',
			// Notify the server about changes to files contained in the workspace
			//fileEvents: workspace.createFileSystemWatcher('**/*.*')
		}
	}
	
	// Create the language client and start the client.
	let disposable = new LanguageClient('Rust Language Service', serverOptions, clientOptions).start();

	// Push the disposable to the context's subscriptions so that the 
	// client can be deactivated on extension deactivation
	context.subscriptions.push(disposable);
}