# 编辑区

编辑区是多标签页的。
标签页下面是编辑器
编辑器分左右两部分：

* 左边：也叫gutter,用于显示行号
* 右边：语法高亮编辑器

## 语法高亮
语法高亮架构选型：tree-sitter+ arborium
语法高亮支持：

1. python
2. rust
3. html/css/javascript. 请注意：大部分html文件里会嵌入js/css代码。
4. markdown,只高亮、不预览

## 标签页图标
参考[icon](./icon.md)

## 图片预览
若打开的是图片，默认以1:1进行图片预览
