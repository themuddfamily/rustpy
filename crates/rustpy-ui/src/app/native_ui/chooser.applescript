on run arguments
    set mode to item 1 of arguments
    set initialFolder to POSIX file (item 2 of arguments) as alias
    set suggestedName to item 3 of arguments
    try
        if mode is "directory" then
            set selectedPath to choose folder with prompt "Open a project folder" default location initialFolder
        else if mode is "save" then
            set selectedPath to choose file name with prompt "Save a copy" default location initialFolder default name suggestedName
        else
            set selectedPath to choose file with prompt "Open a file" default location initialFolder
        end if
        return POSIX path of selectedPath
    on error number -128
        return "RUSTPY_NATIVE_CANCELLED"
    end try
end run
