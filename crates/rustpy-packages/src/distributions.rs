//! Import name → PyPI distribution ("Install package" quick fix for an
//! unresolved import): `import yaml` needs `pip install PyYAML`.

/// `(import name, distribution)` where the two differ beyond case and `_`/`-`
/// (pip normalizes those). Dotted names match a module prefix.
const KNOWN: &[(&str, &str)] = &[
    ("PIL", "Pillow"),
    ("yaml", "PyYAML"),
    ("cv2", "opencv-python"),
    ("sklearn", "scikit-learn"),
    ("skimage", "scikit-image"),
    ("bs4", "beautifulsoup4"),
    ("dateutil", "python-dateutil"),
    ("dotenv", "python-dotenv"),
    ("jwt", "PyJWT"),
    ("jose", "python-jose"),
    ("serial", "pyserial"),
    ("Crypto", "pycryptodome"),
    ("Cryptodome", "pycryptodomex"),
    ("OpenSSL", "pyOpenSSL"),
    ("nacl", "PyNaCl"),
    ("git", "GitPython"),
    ("docx", "python-docx"),
    ("pptx", "python-pptx"),
    ("fitz", "PyMuPDF"),
    ("attr", "attrs"),
    ("cattr", "cattrs"),
    ("google.protobuf", "protobuf"),
    ("google.auth", "google-auth"),
    ("google.oauth2", "google-auth"),
    ("google.api_core", "google-api-core"),
    ("google.api", "googleapis-common-protos"),
    ("google.rpc", "googleapis-common-protos"),
    ("google.type", "googleapis-common-protos"),
    ("google.generativeai", "google-generativeai"),
    ("google.genai", "google-genai"),
    ("google.adk", "google-adk"),
    ("azure.core", "azure-core"),
    ("azure.identity", "azure-identity"),
    ("azure.storage.blob", "azure-storage-blob"),
    ("azure.storage.queue", "azure-storage-queue"),
    ("azure.storage.fileshare", "azure-storage-file-share"),
    ("azure.storage.filedatalake", "azure-storage-file-datalake"),
    ("azure.keyvault.secrets", "azure-keyvault-secrets"),
    ("azure.keyvault.keys", "azure-keyvault-keys"),
    ("azure.keyvault.certificates", "azure-keyvault-certificates"),
    ("azure.cosmos", "azure-cosmos"),
    ("azure.servicebus", "azure-servicebus"),
    ("azure.eventhub", "azure-eventhub"),
    ("azure.functions", "azure-functions"),
    ("azure.ai.ml", "azure-ai-ml"),
    ("azure.ai.openai", "azure-ai-openai"),
    ("googleapiclient", "google-api-python-client"),
    ("grpc", "grpcio"),
    ("MySQLdb", "mysqlclient"),
    ("mysql", "mysql-connector-python"),
    ("bson", "pymongo"),
    ("gridfs", "pymongo"),
    ("memcache", "python-memcached"),
    ("kafka", "kafka-python"),
    ("zmq", "pyzmq"),
    ("dns", "dnspython"),
    ("socks", "PySocks"),
    ("websocket", "websocket-client"),
    ("multipart", "python-multipart"),
    ("slugify", "python-slugify"),
    ("magic", "python-magic"),
    ("ldap", "python-ldap"),
    ("telegram", "python-telegram-bot"),
    ("telebot", "pyTelegramBotAPI"),
    ("discord", "discord.py"),
    ("speech_recognition", "SpeechRecognition"),
    ("ffmpeg", "ffmpeg-python"),
    ("usb", "pyusb"),
    ("wx", "wxPython"),
    ("gi", "PyGObject"),
    ("cairo", "pycairo"),
    ("OpenGL", "PyOpenGL"),
    ("Xlib", "python-xlib"),
    ("win32api", "pywin32"),
    ("win32con", "pywin32"),
    ("win32com", "pywin32"),
    ("win32gui", "pywin32"),
    ("pywintypes", "pywin32"),
    ("RPi", "RPi.GPIO"),
    ("pkg_resources", "setuptools"),
    ("mpl_toolkits", "matplotlib"),
    ("pylab", "matplotlib"),
    ("faiss", "faiss-cpu"),
    ("ruamel", "ruamel.yaml"),
    ("Bio", "biopython"),
    ("osgeo", "GDAL"),
    ("shapefile", "pyshp"),
    ("snappy", "python-snappy"),
    ("rest_framework", "djangorestframework"),
    ("corsheaders", "django-cors-headers"),
    ("debug_toolbar", "django-debug-toolbar"),
    ("crispy_forms", "django-crispy-forms"),
    ("django_filters", "django-filter"),
    ("environ", "django-environ"),
    ("allauth", "django-allauth"),
    ("storages", "django-storages"),
];

/// Namespace packages (PEP 420) shared by many distributions, each of which
/// installs one subpackage, with how that subpackage's distribution is
/// named: `Some(prefix)` → `prefix` + the subpackage name, `None` → only
/// the [`KNOWN`] entries (the naming varies).
const NAMESPACES: &[(&str, Option<&str>)] = &[
    ("google.cloud", Some("google-cloud-")),
    ("google", None),
    ("azure.mgmt", Some("azure-mgmt-")),
    ("azure", None),
    ("zope", Some("zope.")),
    ("jaraco", Some("jaraco.")),
    ("sphinxcontrib", Some("sphinxcontrib-")),
    ("backports", Some("backports.")),
];

/// What provides an import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    /// The distribution to install (`PyYAML`, `google-cloud-firestore`).
    pub distribution: String,
    /// The package that distribution installs (`yaml`, `google.cloud.firestore`).
    pub package: String,
}

/// The distribution that provides `module` (a dotted import path): a known
/// mapping for the longest matching prefix, the naming pattern of a
/// namespace package, else the top-level package name. `None` for an
/// unknown part of a namespace package (`google.colab`): the top-level name
/// would be an unrelated project.
pub fn provider_for_module(module: &str) -> Option<Provider> {
    let module = module.trim();
    let mut prefix = module;
    loop {
        if let Some((_, dist)) = KNOWN.iter().find(|(name, _)| *name == prefix) {
            return Some(Provider {
                distribution: (*dist).to_string(),
                package: prefix.to_string(),
            });
        }
        match prefix.rfind('.') {
            Some(i) => prefix = &prefix[..i],
            None => break,
        }
    }
    let top = module.split('.').next().unwrap_or(module);
    // The longest namespace `module` lies in.
    let namespace = NAMESPACES
        .iter()
        .filter(|(ns, _)| {
            module
                .strip_prefix(ns)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
        })
        .max_by_key(|(ns, _)| ns.len());
    match namespace {
        Some((ns, naming)) => {
            let sub = module[ns.len()..]
                .trim_start_matches('.')
                .split('.')
                .next()
                .filter(|s| !s.is_empty())?;
            let dist_prefix = (*naming)?;
            Some(Provider {
                distribution: format!("{dist_prefix}{}", sub.replace('_', "-")),
                package: format!("{ns}.{sub}"),
            })
        }
        None => Some(Provider {
            distribution: top.to_string(),
            package: top.to_string(),
        }),
    }
}

/// The distribution to install for `module` (see [`provider_for_module`]).
pub fn distribution_for_module(module: &str) -> Option<String> {
    provider_for_module(module).map(|p| p.distribution)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_names_map_to_distributions() {
        let dist = |m: &str| distribution_for_module(m).unwrap_or_else(|| panic!("{m}"));
        assert_eq!(dist("PIL"), "Pillow");
        assert_eq!(dist("PIL.Image"), "Pillow");
        assert_eq!(dist("yaml"), "PyYAML");
        assert_eq!(dist("cv2"), "opencv-python");
        assert_eq!(dist("sklearn.linear_model"), "scikit-learn");
        assert_eq!(dist("bs4"), "beautifulsoup4");
        assert_eq!(dist("google.protobuf.message"), "protobuf");
        assert_eq!(dist("mysql.connector"), "mysql-connector-python");
        // Unknown names install their top-level package.
        assert_eq!(dist("requests"), "requests");
        assert_eq!(dist("numpy.linalg"), "numpy");
    }

    #[test]
    fn namespace_packages_map_to_their_subpackage_distribution() {
        let p = |m: &str| provider_for_module(m).map(|p| (p.distribution, p.package));
        let pair = |d: &str, pkg: &str| Some((d.to_string(), pkg.to_string()));
        assert_eq!(
            p("google.cloud.firestore"),
            pair("google-cloud-firestore", "google.cloud.firestore")
        );
        assert_eq!(
            p("google.cloud.secret_manager.v1"),
            pair("google-cloud-secret-manager", "google.cloud.secret_manager")
        );
        assert_eq!(
            p("google.oauth2.service_account"),
            pair("google-auth", "google.oauth2")
        );
        assert_eq!(
            p("azure.storage.blob.aio"),
            pair("azure-storage-blob", "azure.storage.blob")
        );
        assert_eq!(
            p("azure.mgmt.compute"),
            pair("azure-mgmt-compute", "azure.mgmt.compute")
        );
        assert_eq!(
            p("zope.interface"),
            pair("zope.interface", "zope.interface")
        );
        assert_eq!(
            p("sphinxcontrib.napoleon"),
            pair("sphinxcontrib-napoleon", "sphinxcontrib.napoleon")
        );
        // Unknown parts of a namespace never fall back to the unrelated top-level project.
        for m in [
            "google",
            "google.colab",
            "google.cloud",
            "azure.something",
            "azure",
        ] {
            assert_eq!(p(m), None, "{m}");
        }
        assert_eq!(p("yaml.loader"), pair("PyYAML", "yaml"));
        assert_eq!(
            p("googleapiclient.discovery"),
            pair("google-api-python-client", "googleapiclient")
        );
        assert_eq!(p("requests"), pair("requests", "requests"));
    }
}
