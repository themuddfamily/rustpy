//! Capability-aware protocol workspace shared by Python and configured DAP
//! adapters. Optional features remain disabled when the adapter omits them.
use rustpy_debug::DebugSession;
use serde_json::{json, Value};
pub fn show(ui: &mut egui::Ui, session: &mut DebugSession) -> Option<std::path::PathBuf> {
    let mut navigate = None;
    egui::CollapsingHeader::new("Protocol / Sources / Debug logs").id_salt("debug_protocol_workspace").show(ui,|ui|{
        ui.horizontal_wrapped(|ui|{
            ui.checkbox(&mut session.protocol.logging,"Debug logs");
            if ui.button("Copy logs").clicked(){ui.ctx().copy_text(session.protocol.logs.iter().cloned().collect::<Vec<_>>().join("\n"));}
            if ui.button("Reconnect adapter").clicked(){if let Err(error)=session.reconnect_adapter(){ui.colored_label(egui::Color32::LIGHT_RED,error.to_string());}}
            if ui.add_enabled(session.protocol.supports("supportsTerminateRequest"),egui::Button::new("Terminate debuggee")).clicked(){session.terminate();}
            if ui.add_enabled(session.is_stopped()&&session.protocol.supports("supportsRestartFrame"),egui::Button::new("Restart selected frame")).clicked(){session.restart_selected_frame();}
        });
        if let Some(process)=&session.protocol.process{ui.label(format!("Process: {} ({})",process["name"].as_str().unwrap_or(""),process["systemProcessId"]));}
        egui::CollapsingHeader::new("Capabilities").show(ui,|ui|{ui.monospace(serde_json::to_string_pretty(&session.protocol.capabilities).unwrap_or_default());});
        egui::CollapsingHeader::new("Modules and loaded sources").show(ui,|ui|{
            ui.horizontal(|ui|{
                if ui.add_enabled(session.protocol.supports("supportsModulesRequest"),egui::Button::new("Refresh modules")).clicked(){session.request_modules();}
                if ui.add_enabled(session.protocol.supports("supportsLoadedSourcesRequest"),egui::Button::new("Refresh sources")).clicked(){session.request_loaded_sources();}
                if ui.button("Selected frame source").clicked(){if let Some(frame)=session.frames.get(session.selected_frame){session.request_frame_source(frame.id);}}
            });
            for module in session.protocol.modules.values(){ui.label(format!("{} — {}",module["name"].as_str().unwrap_or("module"),module["path"].as_str().unwrap_or("")));}
            let mut source=None;
            egui::ScrollArea::vertical().id_salt("loaded_debug_sources").max_height(160.0).show(ui,|ui|{
                for item in &session.protocol.sources{if ui.button(item["name"].as_str().or_else(||item["path"].as_str()).unwrap_or("Source")).clicked(){source=Some(item.clone());}}
            });
            if let Some(source)=source{if source["sourceReference"].as_i64().unwrap_or(0)>0{session.request_source(source);}else if let Some(path)=source["path"].as_str(){navigate=Some(session.local_source_path(path));}}
            if let Some((_,(_,content)))=session.protocol.source_open.and_then(|reference|session.protocol.source_contents.get_key_value(&reference)){
                let lines=content.lines().collect::<Vec<_>>();let height=ui.text_style_height(&egui::TextStyle::Monospace);
                egui::ScrollArea::both().id_salt("adapter_source_text").max_height(240.0).show_rows(ui,height,lines.len(),|ui,range|{for index in range{ui.add(egui::Label::new(egui::RichText::new(lines[index]).monospace()).selectable(true));}});
            }
        });
        egui::CollapsingHeader::new("Function and data breakpoints").show(ui,|ui|{
            ui.horizontal(|ui|{
                ui.text_edit_singleline(&mut session.protocol.function_name);
                if ui.add_enabled(session.protocol.supports("supportsFunctionBreakpoints"),egui::Button::new("Add function breakpoint")).clicked(){let name=std::mem::take(&mut session.protocol.function_name);if !name.trim().is_empty(){let mut breakpoints=session.protocol.function_breakpoints.clone();breakpoints.push(json!({"name":name}));session.set_function_breakpoints(breakpoints);}}
                if ui.button("Clear function breakpoints").clicked(){session.set_function_breakpoints(Vec::new());}
            });
            for point in &session.protocol.function_breakpoints{ui.monospace(point.to_string());}
            ui.horizontal(|ui|{
                ui.label("Data name");ui.text_edit_singleline(&mut session.protocol.data_expression);ui.label("Variable reference");ui.add(egui::DragValue::new(&mut session.protocol.data_reference).range(0..=i64::MAX));
                if ui.add_enabled(session.protocol.supports("supportsDataBreakpoints")&&session.is_stopped(),egui::Button::new("Inspect data breakpoint")).clicked(){session.request_data_breakpoint(&session.protocol.data_expression.clone(),session.protocol.data_reference);}
            });
            if let Some(info)=session.protocol.custom_results.get("dataBreakpointInfo").cloned(){
                ui.monospace(info.to_string());
                if let Some(id)=info["dataId"].as_str(){for access in info["accessTypes"].as_array().cloned().unwrap_or_else(||vec![json!("write")]){if let Some(access)=access.as_str(){if ui.button(format!("Break on {access}")).clicked(){let mut breakpoints=session.protocol.data_breakpoints.clone();breakpoints.push(json!({"dataId":id,"accessType":access}));session.set_data_breakpoints(breakpoints);}}}}
            }
            if ui.button("Clear data breakpoints").clicked(){session.set_data_breakpoints(Vec::new());}
            for command in ["setFunctionBreakpoints","setDataBreakpoints"]{if let Some(result)=session.protocol.custom_results.get(command){ui.monospace(format!("{command}: {result}"));}}
        });
        egui::CollapsingHeader::new("Set expression").show(ui,|ui|{
            ui.horizontal(|ui|{ui.label("Expression");ui.text_edit_singleline(&mut session.protocol.expression);ui.label("Value");ui.text_edit_singleline(&mut session.protocol.expression_value);
                if ui.add_enabled(session.protocol.supports("supportsSetExpression")&&session.is_stopped(),egui::Button::new("Set")).clicked(){session.set_expression(&session.protocol.expression.clone(),&session.protocol.expression_value.clone());}
            });
        });
        egui::CollapsingHeader::new("Python adapter properties and source maps").show(ui,|ui|{
            if ui.button("Query pydevd system information").clicked(){session.debugger_system_info();}
            if let Some(info)=session.protocol.custom_results.get("pydevdSystemInfo"){ui.monospace(serde_json::to_string_pretty(info).unwrap_or_default());}
            ui.horizontal(|ui|{ui.label("Access token");ui.add(egui::TextEdit::singleline(&mut session.protocol.authorization_token).password(true));if ui.button("Authorize").clicked(){let token=std::mem::take(&mut session.protocol.authorization_token);session.authorize_pydevd(&token);}});
            if session.protocol.properties_text.is_empty(){session.protocol.properties_text="{\"multiThreadsSingleNotification\": true, \"dontTraceStartPatterns\": [], \"dontTraceEndPatterns\": []}".into();}
            ui.label("Runtime debugger properties (setDebuggerProperty)");ui.text_edit_multiline(&mut session.protocol.properties_text);
            if ui.add_enabled(session.protocol.supports("supportsDebuggerProperties"),egui::Button::new("Apply runtime properties")).clicked(){match serde_json::from_str::<Value>(&session.protocol.properties_text){Ok(value) if value.is_object()=>session.set_debugger_property(value),_=>{ui.colored_label(egui::Color32::LIGHT_RED,"Enter a JSON object");}}}
            if session.protocol.attach_options_text.is_empty(){session.protocol.attach_options_text=serde_json::to_string_pretty(&json!({"justMyCode":session.options.just_my_code,"showReturnValue":session.options.show_return_value,"rules":[],"debugOptions":[],"variablePresentation":{"all":"group","special":"hide","protected":"inline","class":"group","function":"group"}})).unwrap_or_default();}
            ui.label("Attach options (apply on reconnect / restart)");ui.text_edit_multiline(&mut session.protocol.attach_options_text);
            if ui.button("Save attach options for this session").clicked(){match serde_json::from_str::<Value>(&session.protocol.attach_options_text){Ok(value)=>{if let Err(error)=session.configure_attach_options(value){ui.colored_label(egui::Color32::LIGHT_RED,error.to_string());}},Err(error)=>{ui.colored_label(egui::Color32::LIGHT_RED,error.to_string());}}}
            ui.label("Source map local path");ui.text_edit_singleline(&mut session.protocol.source_map_path);
            ui.label("pydevd source mappings JSON [{line,endLine,runtimeLine,runtimeSource:{path}}]");ui.text_edit_multiline(&mut session.protocol.source_maps_text);
            if ui.button("Apply source maps").clicked(){match serde_json::from_str::<Value>(&session.protocol.source_maps_text){Ok(value)=>session.set_source_maps(&session.protocol.source_map_path.clone(),value),Err(error)=>{ui.colored_label(egui::Color32::LIGHT_RED,error.to_string());}}}
        });
        if session.protocol.logging{egui::ScrollArea::vertical().id_salt("debug_protocol_logs").max_height(140.0).show(ui,|ui|{for line in &session.protocol.logs{ui.monospace(line);}});}
    });
    navigate
}
