//! Compatibility facade for asset contracts plus cross-domain runtime loading.

pub use engine_assets::asset::*;

use engine_authoring::id::AssetId;
use hashbrown::HashMap;
use std::path::{Component as PathComponent, Path, PathBuf};

/// Reports why an asset path was rejected before file access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetPathError {
    /// The requested asset path was empty.
    Empty,
    /// The requested asset path was absolute or had a platform path prefix.
    NotRelative,
    /// The requested asset path contained a parent directory segment.
    ParentTraversal,
    /// The resolved path escaped the configured asset root.
    OutsideRoot,
}

impl std::fmt::Display for AssetPathError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("asset path must not be empty"),
            Self::NotRelative => formatter.write_str("asset path must be relative to the asset root"),
            Self::ParentTraversal => formatter.write_str("asset path must not contain parent directory segments"),
            Self::OutsideRoot => formatter.write_str("asset path resolves outside the asset root"),
        }
    }
}
impl std::error::Error for AssetPathError {}

/// Reports a failed cross-domain asset loading operation.
#[derive(Debug)]
pub enum AssetLoadError {
    /// The requested path is not permitted below the configured asset root.
    InvalidPath {
        /// Rejected path.
        path: PathBuf,
        /// Rejection reason.
        source: AssetPathError,
    },
    /// The source file could not be read.
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Texture data could not be decoded or uploaded.
    Texture {
        /// Invalid or unsupported texture path.
        path: PathBuf,
        /// Underlying upload error.
        source: crate::material::TextureUploadError,
    },
    /// Mesh loading is not implemented for the requested file.
    UnsupportedMeshFormat {
        /// Unsupported mesh path.
        path: PathBuf,
    },
    /// Audio loading is not implemented for the requested file.
    UnsupportedAudioFormat {
        /// Unsupported audio path.
        path: PathBuf,
    },
    /// The mesh file could not be parsed.
    MeshParse {
        /// Path that failed to parse.
        path: PathBuf,
        /// Human-readable parse failure.
        message: String,
    },
    /// The audio file could not be decoded.
    AudioDecode {
        /// Path that failed to decode.
        path: PathBuf,
        /// Human-readable decode failure.
        message: String,
    },
    /// A supported asset file was readable but its authored content was invalid.
    InvalidAsset {
        /// Invalid source path.
        path: PathBuf,
        /// Actionable parser or compatibility detail.
        message: String,
    },
}

impl std::fmt::Display for AssetLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPath { path, source } => write!(formatter, "invalid asset path {}: {source}", path.display()),
            Self::Io { path, source } => write!(formatter, "failed to read asset {}: {source}", path.display()),
            Self::Texture { path, source } => write!(formatter, "failed to load texture {}: {source}", path.display()),
            Self::UnsupportedMeshFormat { path } => write!(formatter, "mesh loading is not implemented for {}", path.display()),
            Self::UnsupportedAudioFormat { path } => write!(formatter, "audio loading is not implemented for {}", path.display()),
            Self::MeshParse { path, message } => write!(formatter, "failed to parse mesh {}: {message}", path.display()),
            Self::AudioDecode { path, message } => write!(formatter, "failed to decode audio {}: {message}", path.display()),
            Self::InvalidAsset { path, message } => write!(formatter, "invalid asset {}: {message}", path.display()),
        }
    }
}

impl std::error::Error for AssetLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidPath { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::Texture { source, .. } => Some(source),
            Self::UnsupportedMeshFormat { .. }
            | Self::UnsupportedAudioFormat { .. }
            | Self::MeshParse { .. }
            | Self::AudioDecode { .. }
            | Self::InvalidAsset { .. } => None,
        }
    }
}

/// Loads cross-domain runtime assets from files below a configured root directory.
///
/// Stable handles and manifest ownership live in `engine-assets`; this adapter
/// remains at the composition layer because it coordinates render and audio types.
pub struct AssetServer {
    root: PathBuf,
    mesh_cache: HashMap<AssetId, Handle<crate::mesh::Mesh>>,
    texture_cache: HashMap<AssetId, Handle<std::sync::Arc<crate::material::Texture>>>,
    audio_cache: HashMap<AssetId, Handle<crate::audio::AudioAsset>>,
}

impl AssetServer {
    /// Creates an asset server rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            mesh_cache: HashMap::new(),
            texture_cache: HashMap::new(),
            audio_cache: HashMap::new(),
        }
    }

    /// Creates an asset server rooted at `assets_root`.
    pub fn with_assets_root(path: impl Into<PathBuf>) -> Self {
        Self::new(path)
    }

    /// Returns the configured asset root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the cached mesh handle for `asset_id`, if any.
    pub fn cached_mesh(&self, asset_id: &AssetId) -> Option<Handle<crate::mesh::Mesh>> {
        self.mesh_cache.get(asset_id).copied()
    }

    /// Returns the cached texture handle for `asset_id`, if any.
    pub fn cached_texture(
        &self,
        asset_id: &AssetId,
    ) -> Option<Handle<std::sync::Arc<crate::material::Texture>>> {
        self.texture_cache.get(asset_id).copied()
    }

    /// Returns the cached audio handle for `asset_id`, if any.
    pub fn cached_audio(&self, asset_id: &AssetId) -> Option<Handle<crate::audio::AudioAsset>> {
        self.audio_cache.get(asset_id).copied()
    }

    /// Inserts a pre-loaded mesh handle into the cache under `asset_id`.
    pub fn cache_mesh(&mut self, asset_id: AssetId, handle: Handle<crate::mesh::Mesh>) {
        self.mesh_cache.insert(asset_id, handle);
    }

    /// Inserts a pre-loaded texture handle into the cache under `asset_id`.
    pub fn cache_texture(
        &mut self,
        asset_id: AssetId,
        handle: Handle<std::sync::Arc<crate::material::Texture>>,
    ) {
        self.texture_cache.insert(asset_id, handle);
    }

    /// Inserts a pre-loaded audio handle into the cache under `asset_id`.
    pub fn cache_audio(&mut self, asset_id: AssetId, handle: Handle<crate::audio::AudioAsset>) {
        self.audio_cache.insert(asset_id, handle);
    }

    /// Reads an asset file as bytes relative to the asset root.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is invalid or cannot be read.
    pub fn load_bytes(&self, path: &str) -> Result<Vec<u8>, AssetLoadError> {
        let full_path = self.resolve_path(path)?;
        Self::read_file(&full_path)
    }

    /// Loads an OBJ mesh into `meshes`.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid paths, unsupported formats, I/O, or parse failures.
    pub fn load_mesh(
        &self,
        path: &str,
        meshes: &mut Assets<crate::mesh::Mesh>,
    ) -> Result<Handle<crate::mesh::Mesh>, AssetLoadError> {
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
        if !ext.eq_ignore_ascii_case("obj") {
            return Err(AssetLoadError::UnsupportedMeshFormat { path: PathBuf::from(path) });
        }
        let full_path = self.resolve_path(path)?;
        let mesh = load_obj(&full_path)?;
        Ok(meshes.add(mesh))
    }

    /// Loads a PNG, JPEG, WebP, or BMP texture into `textures`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, decoded, or uploaded.
    pub fn load_texture(
        &self,
        path: &str,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        textures: &mut Assets<std::sync::Arc<crate::material::Texture>>,
    ) -> Result<Handle<std::sync::Arc<crate::material::Texture>>, AssetLoadError> {
        let full_path = self.resolve_path(path)?;
        let bytes = Self::read_file(&full_path)?;
        let texture = crate::material::Texture::from_bytes(device, queue, &bytes, path)
            .map_err(|source| AssetLoadError::Texture {
                path: full_path,
                source,
            })?;
        Ok(textures.add(std::sync::Arc::new(texture)))
    }

    /// Loads a WAV or OGG audio file and caches it by authoring ID.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid paths, unsupported formats, I/O, or decode failures.
    pub fn load_audio(
        &mut self,
        asset_id: AssetId,
        path: &str,
        audio_assets: &mut Assets<crate::audio::AudioAsset>,
    ) -> Result<Handle<crate::audio::AudioAsset>, AssetLoadError> {
        if let Some(handle) = self.cached_audio(&asset_id) {
            return Ok(handle);
        }
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
        if !ext.eq_ignore_ascii_case("wav") && !ext.eq_ignore_ascii_case("ogg") {
            return Err(AssetLoadError::UnsupportedAudioFormat { path: PathBuf::from(path) });
        }
        let full_path = self.resolve_path(path)?;
        let bytes = Self::read_file(&full_path)?;
        let asset = crate::audio::AudioAsset::from_bytes(bytes).map_err(|source| {
            AssetLoadError::AudioDecode {
                path: full_path,
                message: source.to_string(),
            }
        })?;
        let handle = audio_assets.add(asset);
        self.cache_audio(asset_id, handle);
        Ok(handle)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn resolve_path(&self, path: &str) -> Result<PathBuf, AssetLoadError> {
        let requested = Path::new(path);
        if path.is_empty() {
            return Err(Self::invalid_path(requested, AssetPathError::Empty));
        }
        for component in requested.components() {
            match component {
                PathComponent::Prefix(_) | PathComponent::RootDir => {
                    return Err(Self::invalid_path(requested, AssetPathError::NotRelative));
                }
                PathComponent::ParentDir => {
                    return Err(Self::invalid_path(requested, AssetPathError::ParentTraversal));
                }
                PathComponent::CurDir | PathComponent::Normal(_) => {}
            }
        }
        let root = std::fs::canonicalize(&self.root).map_err(|source| AssetLoadError::Io {
            path: self.root.clone(),
            source,
        })?;
        let full_path = self.root.join(requested);
        let resolved = std::fs::canonicalize(&full_path).map_err(|source| AssetLoadError::Io {
            path: full_path,
            source,
        })?;
        if !resolved.starts_with(&root) {
            return Err(Self::invalid_path(requested, AssetPathError::OutsideRoot));
        }
        Ok(resolved)
    }

    #[cfg(target_arch = "wasm32")]
    fn resolve_path(&self, path: &str) -> Result<PathBuf, AssetLoadError> {
        Err(AssetLoadError::Io {
            path: PathBuf::from(path),
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "asset file I/O is not available on wasm32",
            ),
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn read_file(path: &Path) -> Result<Vec<u8>, AssetLoadError> {
        std::fs::read(path).map_err(|source| AssetLoadError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    #[cfg(target_arch = "wasm32")]
    fn read_file(path: &Path) -> Result<Vec<u8>, AssetLoadError> {
        Err(AssetLoadError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "asset file I/O is not available on wasm32",
            ),
        })
    }

    fn invalid_path(path: &Path, source: AssetPathError) -> AssetLoadError {
        AssetLoadError::InvalidPath {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl Default for AssetServer {
    fn default() -> Self {
        Self::new(".")
    }
}

pub(crate) fn load_obj(path: &Path) -> Result<crate::mesh::Mesh, AssetLoadError> {
    let (models, _materials) = tobj::load_obj(
        path,
        &tobj::LoadOptions {
            triangulate: true,
            single_index: true,
            ..Default::default()
        },
    )
    .map_err(|error| AssetLoadError::MeshParse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    let mut vertices: Vec<crate::mesh::Vertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for model in &models {
        let mesh = &model.mesh;
        let vertex_count = mesh.positions.len() / 3;
        let index_offset = vertices.len() as u32;
        for i in 0..vertex_count {
            let position = [
                mesh.positions[3 * i],
                mesh.positions[3 * i + 1],
                mesh.positions[3 * i + 2],
            ];
            let normal = if mesh.normals.is_empty() {
                [0.0, 1.0, 0.0]
            } else {
                [mesh.normals[3 * i], mesh.normals[3 * i + 1], mesh.normals[3 * i + 2]]
            };
            let uv = if mesh.texcoords.is_empty() {
                [0.0, 0.0]
            } else {
                [mesh.texcoords[2 * i], 1.0 - mesh.texcoords[2 * i + 1]]
            };
            vertices.push(crate::mesh::Vertex {
                position,
                normal,
                color: [1.0; 3],
                uv,
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            });
        }
        for &idx in &mesh.indices {
            indices.push(index_offset + idx);
        }
    }
    Ok(crate::mesh::Mesh {
        vertices,
        indices: Some(indices),
        skinning: None,
        tangents: None,
        submeshes: Vec::new(),
    })
}
