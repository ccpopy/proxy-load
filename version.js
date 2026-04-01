const fs = require('fs');
const path = require('path');

/**
 * 版本信息模块
 * 支持开发环境和pkg打包环境
 */

let versionInfo = null;

/**
 * 获取版本信息
 * @returns {Object} 版本信息对象
 */
function getVersion() {
  if (versionInfo) {
    return versionInfo;
  }

  try {
    // 尝试从package.json读取版本信息
    let packageJson;

    // 判断是否在pkg打包环境中
    if (process.pkg) {
      // pkg打包环境：从嵌入的快照中读取
      packageJson = require('./package.json');
    } else {
      // 开发环境：从文件系统读取
      const packagePath = path.join(__dirname, 'package.json');
      const packageContent = fs.readFileSync(packagePath, 'utf8');
      packageJson = JSON.parse(packageContent);
    }

    versionInfo = {
      version: packageJson.version || '1.0.0',
      name: packageJson.name || 'zwfw-load',
      description: packageJson.description || '代理负载均衡管理系统',
      author: packageJson.author || 'linmew',
      buildTime: getBuildTime(),
      environment: process.pkg ? 'production' : 'development',
      nodeVersion: process.version,
      platform: process.platform,
      arch: process.arch
    };

    return versionInfo;
  } catch (error) {
    console.error('读取版本信息失败:', error);

    // 返回默认版本信息
    return {
      version: '1.0.0',
      name: 'zwfw-load',
      description: '代理负载均衡管理系统',
      author: 'linmew',
      buildTime: new Date().toISOString(),
      environment: process.pkg ? 'production' : 'development',
      nodeVersion: process.version,
      platform: process.platform,
      arch: process.arch,
      error: error.message
    };
  }
}

/**
 * 获取构建时间
 * @returns {string} ISO格式的时间字符串
 */
function getBuildTime() {
  try {
    if (process.pkg) {
      // pkg打包环境：使用可执行文件的修改时间
      const stats = fs.statSync(process.execPath);
      return stats.mtime.toISOString();
    } else {
      // 开发环境：使用package.json的修改时间
      const packagePath = path.join(__dirname, 'package.json');
      const stats = fs.statSync(packagePath);
      return stats.mtime.toISOString();
    }
  } catch (error) {
    return new Date().toISOString();
  }
}

/**
 * 打印版本信息到控制台
 */
function printVersion() {
  const info = getVersion();
  console.log('='.repeat(60));
  console.log(`${info.name} v${info.version}`);
  console.log(`${info.description}`);
  console.log('-'.repeat(60));
  console.log(`作者: ${info.author}`);
  console.log(`环境: ${info.environment}`);
  console.log(`Node版本: ${info.nodeVersion}`);
  console.log(`平台: ${info.platform}-${info.arch}`);
  console.log(`构建时间: ${new Date(info.buildTime).toLocaleString('zh-CN')}`);
  console.log('='.repeat(60));
}

module.exports = {
  getVersion,
  printVersion
};
