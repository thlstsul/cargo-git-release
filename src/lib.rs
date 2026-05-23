use anyhow::{Result, anyhow};
use clap::Parser;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(
    name = "git-release",
    author = "thlstsul",
    about = "自动化 Git 项目发布流程",
    long_about = "一个用于自动化 Git 项目发布流程的工具，支持版本号更新、提交、打标签和推送到所有远程仓库。支持 workspace 项目。"
)]
pub struct Cli {
    /// 新版本号 (例如: 1.2.3)
    #[arg(value_name = "VERSION")]
    version: String,

    /// 重新发布版本（如果标签已存在则删除重新创建）
    #[arg(long, short = 'r')]
    re_publish: bool,

    /// 跳过版本号格式验证
    #[arg(long, short = 'f')]
    force: bool,

    /// 提交信息模板，{version} 会被替换为实际版本号
    #[arg(
        long,
        short = 'm',
        default_value = "Release version {version}",
        value_name = "MESSAGE"
    )]
    message: String,

    /// 标签前缀，默认为 'v'
    #[arg(long, default_value = "v", value_name = "PREFIX")]
    tag_prefix: String,

    /// 只更新版本号，不执行 Git 操作
    #[arg(long)]
    dry_run: bool,

    /// 排除更新的 crate 名称（可多次使用）
    #[arg(long, value_name = "CRATE")]
    exclude: Vec<String>,

    /// 只更新指定的 crate（可多次使用），默认更新所有
    #[arg(long, value_name = "CRATE")]
    only: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CargoToml {
    package: Option<CargoPackage>,
    workspace: Option<CargoWorkspace>,
    #[serde(flatten)]
    other: toml::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum PackageVersion {
    Direct(String),
    Workspace { workspace: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CargoPackage {
    name: String,
    version: PackageVersion,
    #[serde(flatten)]
    other: toml::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CargoWorkspace {
    members: Option<Vec<String>>,
    package: Option<WorkspacePackage>,
    #[serde(flatten)]
    other: toml::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspacePackage {
    version: Option<String>,
    #[serde(flatten)]
    other: toml::Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct TauriConfig {
    #[serde(flatten)]
    other: serde_json::Value,
    version: String,
}

pub struct ReleaseTool {
    args: Cli,
    updated_files: Vec<PathBuf>,
}

impl ReleaseTool {
    pub fn new(args: Cli) -> Self {
        Self {
            args,
            updated_files: Vec::new(),
        }
    }

    pub fn run(&mut self) -> Result<()> {
        println!("🚀 开始发布版本: {}", self.args.version);

        // 验证版本号格式
        if !self.args.force {
            self.validate_version_format()?;
        }

        // 1. 检查是否是 git 仓库
        self.check_git_repo()?;

        // 2. 检查工作区是否干净
        if !self.is_working_tree_clean()? {
            return Err(anyhow!("工作区有未提交的更改，请先提交或暂存更改"));
        }

        // 3. 更新版本号
        self.update_versions()?;

        if self.args.dry_run {
            println!("✅ 干运行模式完成 - 更新了以下文件:");
            for file in &self.updated_files {
                println!("   - {}", file.display());
            }
            return Ok(());
        }

        // 4. 提交更改
        self.commit_changes()?;

        // 5. 处理标签
        self.handle_tag()?;

        // 6. 推送到所有远程仓库
        self.push_to_remotes()?;

        println!("✅ 版本发布成功: {}", self.args.version);
        Ok(())
    }

    fn validate_version_format(&self) -> Result<()> {
        let version_re = Regex::new(r"^\d+\.\d+\.\d+(-[a-zA-Z0-9\.]+)?(\+[a-zA-Z0-9\.]+)?$")?;
        if !version_re.is_match(&self.args.version) {
            return Err(anyhow!(
                "版本号格式不正确，请使用语义化版本号 (例如: 1.2.3, 2.0.0-beta.1)\n\
                 使用 --force 跳过此验证"
            ));
        }
        Ok(())
    }

    fn check_git_repo(&self) -> Result<()> {
        let output = run_git_cmd(&["rev-parse", "--is-inside-work-tree"])?;

        if !output.status.success() {
            return Err(anyhow!("当前目录不是 git 仓库"));
        }
        Ok(())
    }

    fn is_working_tree_clean(&self) -> Result<bool> {
        let output = run_git_cmd(&["status", "--porcelain"])?;
        Ok(output.stdout.is_empty())
    }

    fn update_versions(&mut self) -> Result<()> {
        println!("📝 更新版本号...");

        // 检查是否是 workspace 项目
        let root_cargo_path = Path::new("Cargo.toml");
        if root_cargo_path.exists() {
            let content = fs::read_to_string(root_cargo_path)?;
            let cargo: CargoToml = toml::from_str(&content)?;

            if cargo.workspace.is_some() {
                println!("🔍 检测到 workspace 项目，更新所有成员...");
                self.update_workspace_versions()?;
            } else {
                // 单个项目
                self.update_single_crate(root_cargo_path)?;
            }
        } else {
            return Err(anyhow!("未找到 Cargo.toml 文件"));
        }

        // 更新 tauri.conf.json
        self.update_tauri_config()?;
        Self::cargo_check()?;

        println!(
            "✅ 版本号更新完成，共更新 {} 个文件",
            self.updated_files.len()
        );
        Ok(())
    }

    fn cargo_check() -> Result<()> {
        run_cmd(Command::new("cargo").arg("check"))?;
        Ok(())
    }

    fn update_workspace_versions(&mut self) -> Result<()> {
        // 首先更新根 Cargo.toml 中的 workspace.package.version（如果存在）
        self.update_root_workspace_version()?;

        // 查找并更新所有成员的 Cargo.toml
        let cargo_toml_files = self.find_all_cargo_toml()?;

        for cargo_path in cargo_toml_files {
            self.update_single_crate(&cargo_path)?;
        }

        Ok(())
    }

    fn find_all_cargo_toml(&self) -> Result<Vec<PathBuf>> {
        let mut cargo_files = Vec::new();
        let root_cargo = Path::new("Cargo.toml").canonicalize()?;

        for entry in WalkDir::new(".")
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.file_name().and_then(|s| s.to_str()) == Some("Cargo.toml") {
                let full_path = path.canonicalize()?;
                if full_path != root_cargo {
                    cargo_files.push(path.to_path_buf());
                }
            }
        }

        Ok(cargo_files)
    }

    fn update_root_workspace_version(&mut self) -> Result<()> {
        let root_cargo_path = Path::new("Cargo.toml");
        let content = fs::read_to_string(root_cargo_path)?;
        let mut cargo: CargoToml = toml::from_str(&content)?;

        // 更新 workspace.package.version
        let mut old_version = None;
        if let Some(ref mut workspace) = cargo.workspace
            && let Some(ref mut workspace_package) = workspace.package
            && let Some(ref mut version) = workspace_package.version
        {
            old_version = Some(version.clone());
            *version = self.args.version.clone();
        }

        if let Some(old_version) = old_version {
            let new_content = toml::to_string_pretty(&cargo)?;
            fs::write(root_cargo_path, new_content)?;
            self.updated_files.push(root_cargo_path.to_path_buf());
            println!(
                "✅ 更新 workspace 版本: {} -> {}",
                old_version, self.args.version
            );
        }

        Ok(())
    }

    fn update_single_crate(&mut self, cargo_path: &Path) -> Result<()> {
        let content = fs::read_to_string(cargo_path)?;
        let cargo: CargoToml = toml::from_str(&content)?;

        // 检查是否需要跳过此 crate
        if let Some(ref package) = cargo.package {
            let crate_name = &package.name;

            // 检查排除列表
            if !self.args.exclude.is_empty() && self.args.exclude.contains(crate_name) {
                println!("⏭️  跳过 crate: {}", crate_name);
                return Ok(());
            }

            // 检查 only 列表
            if !self.args.only.is_empty() && !self.args.only.contains(crate_name) {
                println!("⏭️  跳过 crate (不在 --only 列表中): {}", crate_name);
                return Ok(());
            }

            // 检查版本是否继承自 workspace
            let old_version = match &package.version {
                PackageVersion::Direct(v) => v.clone(),
                PackageVersion::Workspace { .. } => {
                    println!("⏭️  跳过 crate {} (版本继承自 workspace)", crate_name);
                    return Ok(());
                }
            };

            // 创建新的 CargoToml 结构体来更新版本
            let new_cargo_toml = self.create_updated_cargo_toml(&cargo)?;

            let new_content = toml::to_string_pretty(&new_cargo_toml)?;
            fs::write(cargo_path, new_content)?;
            self.updated_files.push(cargo_path.to_path_buf());

            let relative_path = cargo_path.strip_prefix(".").unwrap_or(cargo_path);
            println!(
                "✅ 更新 {} ({}): {} -> {}",
                relative_path.display(),
                crate_name,
                old_version,
                self.args.version
            );
        }

        Ok(())
    }

    fn create_updated_cargo_toml(&self, cargo: &CargoToml) -> Result<CargoToml> {
        let mut updated = cargo.clone();

        if let Some(ref mut package) = updated.package {
            package.version = PackageVersion::Direct(self.args.version.clone());
        }

        Ok(updated)
    }

    fn update_tauri_config(&mut self) -> Result<()> {
        let tauri_paths = ["tauri.conf.json", "src-tauri/tauri.conf.json"];

        for path in tauri_paths {
            let tauri_path = Path::new(path);
            if tauri_path.exists() {
                let content = fs::read_to_string(tauri_path)?;
                let mut tauri_config: TauriConfig = serde_json::from_str(&content)?;

                let old_version = tauri_config.version.clone();
                tauri_config.version = self.args.version.clone();
                println!("✅ 更新 {}: {} -> {}", path, old_version, self.args.version);

                let new_content = serde_json::to_string_pretty(&tauri_config)?;
                fs::write(tauri_path, new_content)?;
                self.updated_files.push(tauri_path.to_path_buf());
                return Ok(());
            }
        }

        println!("⚠️  未找到 tauri.conf.json，跳过");
        Ok(())
    }

    fn commit_changes(&self) -> Result<()> {
        println!("💾 提交更改...");

        run_git_cmd(&["add", "-A"])?;

        let commit_message = self.args.message.replace("{version}", &self.args.version);
        run_git_cmd(&["commit", "-m", &commit_message])?;

        println!("✅ 提交完成: {}", commit_message);
        Ok(())
    }

    fn handle_tag(&self) -> Result<()> {
        let tag_name = format!("{}{}", self.args.tag_prefix, self.args.version);

        let output = run_git_cmd(&["tag", "-l", &tag_name])?;
        let tag_exists = !output.stdout.is_empty();

        if tag_exists {
            if self.args.re_publish {
                println!("🔄 重新发布版本，删除旧标签...");
                run_git_cmd(&["tag", "-d", &tag_name])?;
                self.delete_remote_tags(&tag_name)?;
            } else {
                return Err(anyhow!(
                    "标签 {} 已存在，使用 --re-publish 重新发布",
                    tag_name
                ));
            }
        }

        println!("🏷️  创建标签: {}", tag_name);
        let message = format!("Version {}", self.args.version);
        run_git_cmd(&["tag", "-a", &tag_name, "-m", &message])?;

        Ok(())
    }

    fn delete_remote_tags(&self, tag_name: &str) -> Result<()> {
        let output = run_git_cmd(&["remote"])?;
        let remotes = String::from_utf8(output.stdout)?;

        for remote in remotes.lines() {
            println!("🗑️  删除远程标签 {}/{}", remote, tag_name);
            let _ = Command::new("git")
                .arg("push")
                .arg(remote)
                .arg("--delete")
                .arg(tag_name)
                .status();
        }

        Ok(())
    }

    fn push_to_remotes(&self) -> Result<()> {
        println!("📤 推送到远程仓库...");

        let output = run_git_cmd(&["remote"])?;
        let remotes = String::from_utf8(output.stdout)?;

        for remote in remotes.lines() {
            println!("⬆️  推送到 {}", remote);
            run_git_cmd(&["push", remote, "HEAD"])?;
            run_git_cmd(&["push", remote, "--tags"])?;
        }

        Ok(())
    }
}

fn run_git_cmd(args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new("git").args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("git 命令失败: {}\n{}", args.join(" "), stderr));
    }
    Ok(output)
}

fn run_cmd(command: &mut Command) -> Result<()> {
    let status = command.status()?;
    if !status.success() {
        return Err(anyhow!("命令执行失败: {:?}", command));
    }
    Ok(())
}
