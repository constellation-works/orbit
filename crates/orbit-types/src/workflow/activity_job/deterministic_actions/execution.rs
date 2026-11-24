macro_rules! execution_actions {
    ($collect:ident, $state:tt) => {
        $collect! {
            $state;
            core {
                AttachScriptFiles = 3 => "attach_script_files",
                InvokeAndWait = 14 => "invoke_and_wait",
                InvokeDetached = 15 => "invoke_detached",
                OrbitToolCall = 17 => "orbit_tool_call",
                PluginToolCall = 19 => "plugin.tool_call",
                Sleep = 41 => "sleep",
            }
            engine {
                LocalShell = 12 => "local_shell",
            }
        }
    };
}

pub(super) use execution_actions;
