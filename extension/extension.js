const vscode = require('vscode');
const { spawn } = require('child_process');

let statusBarItem;

function activate(context) {
  statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  statusBarItem.text = "$(symbol-structure) BlastCode";
  statusBarItem.tooltip = "BlastCode Code-Graph Active";
  statusBarItem.command = "blastcode.stats";
  statusBarItem.show();
  context.subscriptions.push(statusBarItem);

  context.subscriptions.push(
    vscode.commands.registerCommand('blastcode.stats', () => {
      runBlastCommand(['stats']);
    }),
    vscode.commands.registerCommand('blastcode.map', () => {
      runBlastCommand(['map']);
    }),
    vscode.commands.registerCommand('blastcode.index', () => {
      runBlastCommand(['index']);
    })
  );
}

function runBlastCommand(args) {
  const root = vscode.workspace.workspaceFolders ? vscode.workspace.workspaceFolders[0].uri.fsPath : '.';
  const outputChannel = vscode.window.createOutputChannel("BlastCode");
  outputChannel.show();
  outputChannel.appendLine(`> blast ${args.join(' ')} (root: ${root})`);

  const proc = spawn('blast', [...args, '--root', root], { shell: true });
  proc.stdout.on('data', (d) => outputChannel.append(d.toString()));
  proc.stderr.on('data', (d) => outputChannel.append(d.toString()));
}

function deactivate() {}

module.exports = { activate, deactivate };
