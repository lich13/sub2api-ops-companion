import PackagePlugin

@main
struct EmbedAccountCatalog: BuildToolPlugin {
    func createBuildCommands(context: PluginContext, target: Target) async throws -> [Command] {
        let root = context.package.directoryURL.deletingLastPathComponent().deletingLastPathComponent()
        let input = root.appending(path: "server/src/account_creation_fields.json")
        let output = context.pluginWorkDirectoryURL.appending(path: "AccountCreationCatalogGenerated.swift")
        return [.buildCommand(
            displayName: "Embed account creation catalog",
            executable: .init(fileURLWithPath: "/usr/bin/python3"),
            arguments: ["-c", ###"""
                import pathlib, sys
                source, dest = map(pathlib.Path, sys.argv[1:])
                dest.write_text('enum AccountCreationCatalogGenerated {\n    static let json = #\"\"\"\n'
                                + source.read_text() + '\n\"\"\"#\n}\n')
                """###, input.path, output.path],
            inputFiles: [input],
            outputFiles: [output]
        )]
    }
}
