// 反向标签 Trie 实现泛域名匹配
// 支持格式: 精确域名 "api.example.com", 泛域名 "*.example.com", 全局通配 "*"

class TrieNode {
  constructor () {
    this.children = new Map();
    this.groupId = null;       // 精确匹配到此节点的 groupId
    this.wildcardGroupId = null; // 通配符 * 匹配的 groupId
  }
}

class DomainMatcher {
  constructor () {
    this.root = new TrieNode();
    this.groupMembers = new Map(); // groupId -> Set<proxyId>
    this.defaultGroupId = null;
    this.defaultMembers = null;    // Set<proxyId>
  }

  /**
   * 从数据库加载所有分组规则，构建 Trie
   */
  async loadFromDb (db) {
    this.root = new TrieNode();
    this.groupMembers = new Map();
    this.defaultGroupId = null;
    this.defaultMembers = null;

    const groups = await db.all('SELECT * FROM proxy_groups WHERE enabled = 1');

    for (const group of groups) {
      // 加载成员
      const members = await db.all(
        'SELECT proxy_id FROM proxy_group_members WHERE group_id = ?',
        [group.id]
      );
      if (members.length > 0) {
        this.groupMembers.set(group.id, new Set(members.map(m => m.proxy_id)));
      }

      // 默认组
      if (group.is_default) {
        this.defaultGroupId = group.id;
        this.defaultMembers = this.groupMembers.get(group.id) || null;
      }

      // 加载域名规则并插入 Trie
      const domains = await db.all(
        'SELECT domain FROM proxy_group_domains WHERE group_id = ?',
        [group.id]
      );
      for (const { domain } of domains) {
        this._insert(domain.toLowerCase(), group.id);
      }
    }
  }

  /**
   * 插入一条域名规则到 Trie
   * 域名标签反向插入: "*.example.com" -> ["com", "example", "*"]
   */
  _insert (domain, groupId) {
    // 全局通配 "*"
    if (domain === '*') {
      this.root.wildcardGroupId = groupId;
      return;
    }

    const labels = domain.split('.').reverse();
    let node = this.root;

    for (let i = 0; i < labels.length; i++) {
      const label = labels[i];

      if (label === '*') {
        // 泛域名: 在当前节点标记 wildcardGroupId
        node.wildcardGroupId = groupId;
        return;
      }

      if (!node.children.has(label)) {
        node.children.set(label, new TrieNode());
      }
      node = node.children.get(label);
    }

    // 精确匹配
    node.groupId = groupId;
  }

  /**
   * 匹配域名，返回 Set<proxyId> 或 null
   * 优先级: 精确匹配 > 更深层泛域名 > 浅层泛域名 > 默认组
   */
  match (domain) {
    if (!domain) return this.defaultMembers;

    const labels = domain.toLowerCase().split('.').reverse();
    let node = this.root;
    let bestWildcardGroupId = node.wildcardGroupId; // 全局 "*" 兜底

    for (const label of labels) {
      // 检查当前节点的通配符子节点
      if (node.children.has('*')) {
        const starNode = node.children.get('*');
        if (starNode.groupId !== null) {
          bestWildcardGroupId = starNode.groupId;
        }
      }

      if (node.children.has(label)) {
        node = node.children.get(label);
        // 如果此节点有 wildcardGroupId，更新最佳匹配
        if (node.wildcardGroupId !== null) {
          bestWildcardGroupId = node.wildcardGroupId;
        }
      } else {
        // 无法继续精确匹配，使用最近的泛域名匹配
        return this._resolveGroup(bestWildcardGroupId);
      }
    }

    // 完整遍历完毕，优先精确匹配
    if (node.groupId !== null) {
      return this._resolveGroup(node.groupId);
    }

    // 回退到泛域名
    return this._resolveGroup(bestWildcardGroupId);
  }

  _resolveGroup (groupId) {
    if (groupId !== null) {
      const members = this.groupMembers.get(groupId);
      if (members && members.size > 0) return members;
    }
    return this.defaultMembers;
  }
}

module.exports = DomainMatcher;
