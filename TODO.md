# To-Do
- Add an endpoint to get the original source of a source topic (a function, a struct definition, a statement, etc) so the user can verify that the formatted version did not miss any information. Include some lines above it to show heading comments. This endpoint can probably do a full read to disk for each request, so no need to be preprocessed, but it will require the either the back end to keep track of a source topic's SourceLocation in the code, or the front end to keep track of it.
- Look into Laiden graph clustering, then only pull other nodes in the community into the context, cutting down on explosive context. Then, community bridging nodes can be marked and scrutinized as so. More below: 
"""
### So what's the real benefit over the raw graph?

 Think of it as a lens, not a filter:

 1. Prioritization — The raw graph has thousands of edges. Communities let analyze.py say "this edge is surprising because it connects nodes from two clusters that otherwise have
 nothing to do with each other." Without that, every cross-file edge looks equally interesting.
 2. Navigation — For humans and agents, "start with Community 0 (Authentication), then look at its bridge to Community 3 (Database)" is vastly more useful than "here are 2,000
 unorganized nodes." The wiki, Obsidian vault, and MCP server all use communities as the primary navigation structure.
 3. Quality signals — Low cohesion scores flag that a community is internally weakly connected, which means either the extraction missed edges or the clustering found something
 artificial. High betweenness bridge nodes flag cross-cutting concerns that might deserve architectural attention.

 TL;DR: Clustering doesn't alter the graph's topology — it adds a community attribute to each node. But that attribute is the primary input to surprise scoring, question
 generation, gap detection, and all structured navigation outputs. The raw graph tells you what's connected; clustering tells you which connections are worth paying attention to.
 
It helps these questions:
 Bridge node questions - Identifies nodes with high betweenness centrality that connect different communities — asks "why does X connect Community A to Community B?"
 
Low cohesion questions - Computes cohesion_score() per community (intra-community edges / max possible) and flags communities below 0.15 as candidates for splitting
 """
 - Use a mechanical semantic linking layer first, then have the LLM apply the links to each definition.
